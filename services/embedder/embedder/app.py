"""FastAPI app surface.

Endpoints:
- GET  /healthz       — liveness + model_loaded probe (reports backend dim)
- POST /embed/audio   — raw bytes → dim-shaped float vector
- POST /embed/text    — JSON {text} → dim-shaped float vector

The embedder backend is wired via a FastAPI dependency so tests can
inject a stub. `build_app()` returns a fresh app with a default
backend selected from the `EMBEDDER_BACKEND` env var. Vector dim is
read from the backend (`emb.dim`) rather than hardcoded so the
sidecar can host CLAP (512) or CLaMP 3 (768) without code changes.
"""

from __future__ import annotations

import asyncio
import base64
import binascii
import os
import re
from typing import Annotated, Mapping

import numpy as np
from fastapi import Depends, FastAPI, HTTPException, Request, Response
from pydantic import BaseModel, Field

from embedder import reduce as reduce_module
from embedder.protocol import Embedder

# Upper bounds on request size. The only caller of these endpoints is the
# gateway (single-user, ~10⁴ tracks), so these are guardrails against a
# gateway bug or a hostile LAN peer exhausting memory — deliberately loose,
# not tight limits.
MAX_AUDIO_BYTES = 64 * 1024 * 1024  # 64 MiB — CLaMP 3 reads the first ~120 s
MAX_REDUCE_ROWS = 50_000  # well above a single-user library
MAX_REDUCE_DIM = 4096  # CLAP=512, CLaMP 3=768
MAX_TEXT_CHARS = 4096  # station queries are short

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
    text: str = Field(
        ...,
        max_length=MAX_TEXT_CHARS,
        description="Free-form text. Empty string is allowed.",
    )


class EmbedResponse(BaseModel):
    vector: list[float]
    dim: int
    model_version: str


class ReduceRequest(BaseModel):
    """Vectors-over-the-wire reduction request.

    The embedder no longer opens the gateway's SQLite file (that assumed
    a shared volume, which breaks when the embedder runs on a separate
    GPU host). Instead the gateway reads its own recommend DB, packs the
    `(N, dim)` embedding matrix as base64 row-major little-endian f32 —
    the exact byte layout of the `vector` blobs — and ships it here. The
    embedder computes coordinates and returns them; the gateway persists
    the projection rows itself.
    """

    track_ids: list[str] = Field(
        ...,
        max_length=MAX_REDUCE_ROWS,
        description="One id per matrix row, defining row order.",
    )
    dim: int = Field(
        ..., gt=0, le=MAX_REDUCE_DIM, description="Embedding dimensionality D."
    )
    vectors_b64: str = Field(
        ...,
        description="base64 of N*D little-endian float32, row-major "
        "(np.frombuffer(dtype='<f4')).",
    )
    n_neighbors: int = reduce_module.DEFAULT_N_NEIGHBORS
    min_dist: float = reduce_module.DEFAULT_MIN_DIST
    random_state: int = reduce_module.DEFAULT_RANDOM_STATE
    n_components: int = Field(2, ge=2, le=3)
    metric: str = reduce_module.DEFAULT_METRIC


class ReducePoint(BaseModel):
    track_id: str
    x: float
    y: float
    z: float | None = None
    pc1: float | None = None
    pc2: float | None = None
    pc3: float | None = None
    pc4: float | None = None


class ReduceResponse(BaseModel):
    points: list[ReducePoint]


# --- backend selection ------------------------------------------------------


def _require_env(name: str, backend: str) -> str:
    """Read a required env var, raising a clear startup error if absent.

    `_default_embedder` runs at import time (`app = build_app()`), so a
    bare `os.environ[...]` KeyError would abort the worker with an opaque
    traceback. This names the missing var and the backend that needs it.
    """
    val = os.environ.get(name)
    if not val:
        raise RuntimeError(
            f"{name} must be set when EMBEDDER_BACKEND={backend!r}"
        )
    return val


def _default_embedder() -> Embedder:
    """Construct the default backend per the EMBEDDER_BACKEND env var.

    - `stub` (default): no model load, suitable for dev. Optional
      `EMBEDDER_STUB_DIM` overrides the vector dim (default 512) so the
      stub can stand in for either backend's wire shape during dev.
    - `clap`: LAION CLAP via the `clap` extra. Requires `CLAP_CHECKPOINT`.
    - `clamp3`: CLaMP 3 via the `clamp3` extra and the vendored
      `embedder._clamp3` package. Requires `CLAMP3_CHECKPOINT` (path to
      the unified saas `.pth`) and `MERT_FOLDER` (path to a local copy
      of `m-a-p/MERT-v1-95M`, or the HF hub id if you want the model
      to download on first run — discouraged for prod because the
      container's unprivileged user has no writable HF cache).
    """
    backend = os.environ.get("EMBEDDER_BACKEND", "stub").lower()
    if backend == "stub":
        from embedder.stub import DEFAULT_DIM, StubEmbedder

        dim_override = os.environ.get("EMBEDDER_STUB_DIM")
        if dim_override:
            try:
                dim = int(dim_override)
            except ValueError:
                raise RuntimeError(
                    f"EMBEDDER_STUB_DIM={dim_override!r} is not a valid integer"
                ) from None
        else:
            dim = DEFAULT_DIM
        return StubEmbedder(model_version="stub-v1", loaded=True, dim=dim)
    if backend == "clap":
        # Lazy import: only pull torch / laion-clap when actually requested.
        from embedder.clap_backend import ClapEmbedder

        ckpt = _require_env("CLAP_CHECKPOINT", backend)
        return ClapEmbedder(checkpoint_path=ckpt)
    if backend == "clamp3":
        # Lazy import: only pull torch / transformers when requested.
        from embedder.clamp3_backend import Clamp3Embedder

        ckpt = _require_env("CLAMP3_CHECKPOINT", backend)
        mert = _require_env("MERT_FOLDER", backend)
        return Clamp3Embedder(checkpoint_path=ckpt, mert_folder=mert)
    raise RuntimeError(f"unknown EMBEDDER_BACKEND={backend!r}")


def get_embedder(request: Request) -> Embedder:
    """FastAPI dependency. Tests override this via `app.dependency_overrides`."""
    return request.app.state.embedder


EmbedderDep = Annotated[Embedder, Depends(get_embedder)]


def _require_bearer(request: Request) -> None:
    """FastAPI dependency guarding privileged endpoints.

    If `app.state.bearer_token` is None (the default — no
    `EMBEDDER_BEARER_TOKEN` in the env at build time), this is a
    no-op. When configured, requires `Authorization: Bearer <token>`
    on the incoming request and raises 401 otherwise.

    Deliberately not applied to /healthz — boot probes shouldn't need
    to be told the secret, and 200/503 on /healthz doesn't reveal
    anything compute-y. /embed/* and /reduce both fan out to the
    model and/or open caller-supplied files, so they're the actual
    privileged surface.
    """
    token = request.app.state.bearer_token
    if token is None:
        return
    header = request.headers.get("authorization", "")
    expected = f"Bearer {token}"
    # Constant-time compare: avoids leaking token length / common-prefix
    # info via response timing. The header is short so the cost is
    # negligible either way; this is the cheap correct thing.
    import hmac

    if not hmac.compare_digest(header, expected):
        raise HTTPException(status_code=401, detail="unauthorized")


BearerDep = Annotated[None, Depends(_require_bearer)]


# --- app factory ------------------------------------------------------------


def build_app(embedder: Embedder | None = None) -> FastAPI:
    """Construct a fresh FastAPI app, optionally with a specific backend."""
    app = FastAPI(title="music-embedder", version="0.1.0")
    app.state.embedder = embedder or _default_embedder()
    # Optional bearer-token gate for split-host deployments (gateway and
    # embedder on different machines, embedder reachable on the LAN).
    # Empty string is treated as "unset" so an accidentally empty env
    # var doesn't quietly accept everyone with `Bearer `.
    token = os.environ.get("EMBEDDER_BEARER_TOKEN", "").strip()
    app.state.bearer_token = token or None

    @app.get("/healthz", response_model=HealthResponse)
    def healthz(emb: EmbedderDep, response: Response) -> HealthResponse:
        loaded = emb.loaded
        if not loaded:
            response.status_code = 503
        return HealthResponse(
            status="ok" if loaded else "loading",
            model_loaded=loaded,
            model_version=emb.model_version,
            dim=emb.dim,
            device=emb.device,
        )

    @app.post("/embed/audio")
    async def embed_audio(req: Request, emb: EmbedderDep, _: BearerDep) -> Response:
        # Inference runs in the default threadpool so concurrent calls
        # don't block the event loop. The actual GPU forward pass still
        # serializes on the device (only one CUDA/HIP stream by
        # default), but decode + resample on the CPU side can overlap
        # across requests, which is what 8 concurrent ingest workers
        # need.
        if not emb.loaded:
            raise HTTPException(status_code=503, detail="model not loaded")
        # Reject oversized payloads up front: trust the declared
        # Content-Length when present, then re-check the buffered length
        # in case the header lied (chunked / no length).
        declared = req.headers.get("content-length")
        if declared is not None and declared.isdigit() and int(declared) > MAX_AUDIO_BYTES:
            raise HTTPException(status_code=413, detail="audio payload too large")
        body = await req.body()
        if len(body) == 0:
            raise HTTPException(status_code=400, detail="empty body")
        if len(body) > MAX_AUDIO_BYTES:
            raise HTTPException(status_code=413, detail="audio payload too large")
        result = await asyncio.to_thread(emb.embed_audio, body)
        return _build_embed_response(result, emb.model_version, emb.dim)

    @app.post("/embed/text")
    async def embed_text(payload: EmbedTextRequest, emb: EmbedderDep, _: BearerDep) -> Response:
        if not emb.loaded:
            raise HTTPException(status_code=503, detail="model not loaded")
        result = await asyncio.to_thread(emb.embed_text, payload.text)
        return _build_embed_response(result, emb.model_version, emb.dim)

    @app.post("/reduce", response_model=ReduceResponse)
    async def reduce(payload: ReduceRequest, _: BearerDep) -> ReduceResponse:
        # Pure compute: decode the matrix, run UMAP+PCA, return coords.
        # No model needed (this isn't inference) and no filesystem access
        # (the gateway owns reading vectors and persisting projections),
        # so intentionally no `emb.loaded` guard.
        n = len(payload.track_ids)
        try:
            raw = base64.b64decode(payload.vectors_b64, validate=True)
        except (ValueError, binascii.Error) as e:
            raise HTTPException(
                status_code=400, detail=f"invalid base64 in vectors_b64: {e}"
            ) from e
        expected = n * payload.dim * 4
        if len(raw) != expected:
            raise HTTPException(
                status_code=400,
                detail=(
                    f"vectors_b64 decodes to {len(raw)} bytes, expected "
                    f"{expected} (= {n} track_ids * {payload.dim} dim * 4)"
                ),
            )
        # Row-major little-endian f32 — the gateway packs the `vector`
        # blobs verbatim. `.reshape` on a zero-row matrix is well-defined.
        matrix = np.frombuffer(raw, dtype="<f4").reshape(n, payload.dim)
        try:
            projections = await asyncio.to_thread(
                reduce_module.project_matrix,
                payload.track_ids,
                matrix,
                n_neighbors=payload.n_neighbors,
                min_dist=payload.min_dist,
                random_state=payload.random_state,
                metric=payload.metric,
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
        except ValueError as e:
            # Bad knobs (n_components ∉ {2,3}, an unknown UMAP `metric`,
            # n_neighbors ≥ N) raise ValueError from the reducer. That's
            # a caller bug, not a server fault — surface a 400 rather
            # than an opaque 500.
            raise HTTPException(status_code=400, detail=str(e)) from e
        return ReduceResponse(
            points=[
                ReducePoint(
                    track_id=p.track_id,
                    x=p.x,
                    y=p.y,
                    z=p.z,
                    pc1=p.pc1,
                    pc2=p.pc2,
                    pc3=p.pc3,
                    pc4=p.pc4,
                )
                for p in projections
            ]
        )

    return app


def _build_embed_response(result, model_version: str, dim: int) -> Response:
    """Wrap an `EmbedResult` in a JSON response and attach the
    `Server-Timing` header. Pulled out of the handlers so the two
    endpoints share the same envelope + header logic. `dim` comes from
    the backend so a CLAP and CLaMP 3 deployment can share this code.
    """
    from fastapi.responses import JSONResponse

    payload = EmbedResponse(
        vector=[float(x) for x in result.vector.tolist()],
        dim=dim,
        model_version=model_version,
    )
    headers: dict[str, str] = {}
    timing = format_server_timing(result.stages_ms)
    if timing:
        headers["Server-Timing"] = timing
    return JSONResponse(content=payload.model_dump(), headers=headers)


# Module-level app for `uvicorn embedder.app:app` deployments.
app = build_app()
