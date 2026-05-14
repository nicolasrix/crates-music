"""FastAPI app surface.

Endpoints:
- GET  /healthz       — liveness + model_loaded probe
- POST /embed/audio   — raw bytes → 512-dim float vector
- POST /embed/text    — JSON {text} → 512-dim float vector

The embedder backend is wired via a FastAPI dependency so tests can
inject a stub. `build_app()` returns a fresh app with a default
backend selected from the `EMBEDDER_BACKEND` env var.
"""

from __future__ import annotations

import asyncio
import os
import re
from pathlib import Path
from typing import Annotated, Mapping

from fastapi import Depends, FastAPI, HTTPException, Request, Response
from pydantic import BaseModel, Field

from embedder import reduce as reduce_module
from embedder.protocol import Embedder

EMBEDDING_DIM: int = 512

# Server-Timing metric names must be HTTP tokens (RFC 7230). Anything
# outside this set means the backend gave us a bad name; we drop the
# entry rather than risk a malformed header that breaks parsers
# downstream.
_TOKEN_RE = re.compile(r"^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$")


def format_server_timing(stages_ms: Mapping[str, float]) -> str:
    """Format a stage timing map as a `Server-Timing` header value.

    `{"decode": 42.0, "gpu_forward": 520.0}` →
    `"decode;dur=42, gpu_forward;dur=520"`.

    Durations round to one decimal so sub-millisecond stages don't get
    silently zeroed but the header stays compact. Whole-millisecond
    values render without the trailing `.0`. Entries with non-token
    names are skipped — see `_TOKEN_RE`.
    """
    parts: list[str] = []
    for name, dur in stages_ms.items():
        if not _TOKEN_RE.match(name):
            continue
        rounded = round(float(dur), 1)
        # Drop trailing ".0" for whole milliseconds — purely cosmetic;
        # parsers accept both forms per the RFC.
        formatted = f"{int(rounded)}" if rounded == int(rounded) else f"{rounded}"
        parts.append(f"{name};dur={formatted}")
    return ", ".join(parts)


# --- request / response models ---------------------------------------------


class HealthResponse(BaseModel):
    status: str
    model_loaded: bool
    model_version: str
    dim: int
    device: str


class EmbedTextRequest(BaseModel):
    text: str = Field(..., description="Free-form text. Empty string is allowed.")


class EmbedResponse(BaseModel):
    vector: list[float]
    dim: int
    model_version: str


class ReduceRequest(BaseModel):
    """Arguments mirror `embedder.reduce.run` exactly. The gateway is
    the only intended caller; it owns the recommend SQLite file and
    passes its filesystem path so the embedder can open the same DB
    directly (single-host deployment)."""

    db_path: str = Field(
        ...,
        description="Absolute path to the recommend SQLite file. The "
        "embedder opens it read+write; the gateway is expected to be "
        "running on the same host.",
    )
    model_version: str
    proj_version: str | None = None
    n_neighbors: int = reduce_module.DEFAULT_N_NEIGHBORS
    min_dist: float = reduce_module.DEFAULT_MIN_DIST
    random_state: int = reduce_module.DEFAULT_RANDOM_STATE
    n_components: int = 2


class ReduceResponse(BaseModel):
    proj_version: str
    written: int


# --- backend selection ------------------------------------------------------


def _default_embedder() -> Embedder:
    """Construct the default backend per the EMBEDDER_BACKEND env var.

    `stub` (default) is loaded; suitable for dev. `clap` requires the
    `clap` optional extra and a CLAP_CHECKPOINT path.
    """
    backend = os.environ.get("EMBEDDER_BACKEND", "stub").lower()
    if backend == "stub":
        from embedder.stub import StubEmbedder

        return StubEmbedder(model_version="stub-v1", loaded=True)
    if backend == "clap":
        # Lazy import: only pull torch / laion-clap when actually requested.
        from embedder.clap_backend import ClapEmbedder

        ckpt = os.environ["CLAP_CHECKPOINT"]
        return ClapEmbedder(checkpoint_path=ckpt)
    raise RuntimeError(f"unknown EMBEDDER_BACKEND={backend!r}")


def get_embedder(request: Request) -> Embedder:
    """FastAPI dependency. Tests override this via `app.dependency_overrides`."""
    return request.app.state.embedder


EmbedderDep = Annotated[Embedder, Depends(get_embedder)]


# --- app factory ------------------------------------------------------------


def build_app(embedder: Embedder | None = None) -> FastAPI:
    """Construct a fresh FastAPI app, optionally with a specific backend."""
    app = FastAPI(title="music-embedder", version="0.1.0")
    app.state.embedder = embedder or _default_embedder()

    @app.get("/healthz", response_model=HealthResponse)
    def healthz(emb: EmbedderDep, response: Response) -> HealthResponse:
        loaded = emb.loaded
        if not loaded:
            response.status_code = 503
        return HealthResponse(
            status="ok" if loaded else "loading",
            model_loaded=loaded,
            model_version=emb.model_version,
            dim=EMBEDDING_DIM,
            device=emb.device,
        )

    @app.post("/embed/audio")
    async def embed_audio(req: Request, emb: EmbedderDep) -> Response:
        # Inference runs in the default threadpool so concurrent calls
        # don't block the event loop. The actual GPU forward pass still
        # serializes on the device (only one CUDA/HIP stream by
        # default), but decode + resample on the CPU side can overlap
        # across requests, which is what 8 concurrent ingest workers
        # need.
        if not emb.loaded:
            raise HTTPException(status_code=503, detail="model not loaded")
        body = await req.body()
        if len(body) == 0:
            raise HTTPException(status_code=400, detail="empty body")
        result = await asyncio.to_thread(emb.embed_audio, body)
        return _build_embed_response(result, emb.model_version)

    @app.post("/embed/text")
    async def embed_text(payload: EmbedTextRequest, emb: EmbedderDep) -> Response:
        if not emb.loaded:
            raise HTTPException(status_code=503, detail="model not loaded")
        result = await asyncio.to_thread(emb.embed_text, payload.text)
        return _build_embed_response(result, emb.model_version)

    @app.post("/reduce", response_model=ReduceResponse)
    async def reduce(payload: ReduceRequest) -> ReduceResponse:
        # The reducer doesn't need the CLAP model — it reads stored
        # embeddings from SQLite. Intentionally no `emb.loaded` guard.
        db_path = Path(payload.db_path)
        if not db_path.exists():
            raise HTTPException(
                status_code=400,
                detail=f"db_path does not exist: {db_path}",
            )
        try:
            pv, written = await asyncio.to_thread(
                reduce_module.run,
                db_path=db_path,
                model_version=payload.model_version,
                n_neighbors=payload.n_neighbors,
                min_dist=payload.min_dist,
                random_state=payload.random_state,
                proj_version=payload.proj_version,
                n_components=payload.n_components,
            )
        except ImportError as e:
            # The `reduce` extra (umap-learn + sklearn) is optional;
            # surface its absence as 503 so the gateway can degrade
            # cleanly rather than treating it as a generic 5xx.
            raise HTTPException(
                status_code=503,
                detail=f"reduce extra not installed (umap/sklearn): {e}",
            ) from e
        return ReduceResponse(proj_version=pv, written=written)

    return app


def _build_embed_response(result, model_version: str) -> Response:
    """Wrap an `EmbedResult` in a JSON response and attach the
    `Server-Timing` header. Pulled out of the handlers so the two
    endpoints share the same envelope + header logic.
    """
    from fastapi.responses import JSONResponse

    payload = EmbedResponse(
        vector=[float(x) for x in result.vector.tolist()],
        dim=EMBEDDING_DIM,
        model_version=model_version,
    )
    headers: dict[str, str] = {}
    timing = format_server_timing(result.stages_ms)
    if timing:
        headers["Server-Timing"] = timing
    return JSONResponse(content=payload.model_dump(), headers=headers)


# Module-level app for `uvicorn embedder.app:app` deployments.
app = build_app()
