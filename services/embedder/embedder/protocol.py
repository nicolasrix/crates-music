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

    @property
    def device(self) -> str:
        """Compute device the model is running on: "cpu" or "cuda".

        On ROCm-built PyTorch, HIP devices identify as "cuda" — so this
        being "cuda" with an AMD card means GPU acceleration is engaged.
        Surfaced via /healthz so the gateway boot probe (and humans) can
        detect silent CPU fallback after a ROCm install.
        """
        ...

    def embed_audio(self, raw_bytes: bytes) -> np.ndarray:
        """Return a 1-D float32 ndarray of shape (EMBEDDING_DIM,)."""
        ...

    def embed_text(self, text: str) -> np.ndarray:
        """Return a 1-D float32 ndarray of shape (EMBEDDING_DIM,)."""
        ...
