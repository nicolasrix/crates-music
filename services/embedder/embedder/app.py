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
from typing import Annotated

from fastapi import Depends, FastAPI, HTTPException, Request, Response
from pydantic import BaseModel, Field

from embedder.protocol import Embedder

EMBEDDING_DIM: int = 512


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

    @app.post("/embed/audio", response_model=EmbedResponse)
    async def embed_audio(req: Request, emb: EmbedderDep) -> EmbedResponse:
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
        vector = await asyncio.to_thread(emb.embed_audio, body)
        return EmbedResponse(
            vector=[float(x) for x in vector.tolist()],
            dim=EMBEDDING_DIM,
            model_version=emb.model_version,
        )

    @app.post("/embed/text", response_model=EmbedResponse)
    async def embed_text(payload: EmbedTextRequest, emb: EmbedderDep) -> EmbedResponse:
        if not emb.loaded:
            raise HTTPException(status_code=503, detail="model not loaded")
        vector = await asyncio.to_thread(emb.embed_text, payload.text)
        return EmbedResponse(
            vector=[float(x) for x in vector.tolist()],
            dim=EMBEDDING_DIM,
            model_version=emb.model_version,
        )

    return app


# Module-level app for `uvicorn embedder.app:app` deployments.
app = build_app()
