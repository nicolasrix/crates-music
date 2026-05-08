"""Embedder sidecar — FastAPI service exposing CLAP audio + text embeddings."""

from embedder.app import build_app

__all__ = ["build_app"]
