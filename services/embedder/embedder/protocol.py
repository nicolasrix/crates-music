"""Embedder protocol: any backend that can embed raw audio bytes and text.

Two implementations:
- `ClapEmbedder` (production, gated behind the `clap` extra) — wraps LAION CLAP.
- `StubEmbedder` (tests + degraded-mode fallback) — deterministic hash-based.

Both produce L2-normalized 512-dim float32 vectors.
"""

from __future__ import annotations

from typing import Protocol

import numpy as np


class Embedder(Protocol):
    """Anything that can produce CLAP-shaped embeddings."""

    @property
    def model_version(self) -> str: ...

    @property
    def loaded(self) -> bool:
        """Whether the underlying weights are loaded and ready to serve."""
        ...

    def embed_audio(self, raw_bytes: bytes) -> np.ndarray:
        """Return a 1-D float32 ndarray of shape (EMBEDDING_DIM,)."""
        ...

    def embed_text(self, text: str) -> np.ndarray:
        """Return a 1-D float32 ndarray of shape (EMBEDDING_DIM,)."""
        ...
