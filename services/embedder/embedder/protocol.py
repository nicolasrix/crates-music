"""Embedder protocol: any backend that can embed raw audio bytes and text.

Two implementations:
- `ClapEmbedder` (production, gated behind the `clap` extra) — wraps LAION CLAP.
- `StubEmbedder` (tests + degraded-mode fallback) — deterministic hash-based.

Both produce L2-normalized 512-dim float32 vectors AND a per-stage
timing breakdown in milliseconds, so the gateway can attribute time
inside `embed_audio` / `embed_text` to its sub-steps via the
`Server-Timing` HTTP response header.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Mapping, Protocol

import numpy as np


@dataclass(frozen=True)
class EmbedResult:
    """A backend's reply: the vector plus per-stage timings.

    `stages_ms` keys are stage names (e.g. "decode", "resample",
    "gpu_forward"); values are wall-clock duration in milliseconds.
    Names should be HTTP token-safe (no spaces, no commas) so they
    survive the trip through `Server-Timing` unchanged. The
    `format_server_timing` helper drops bad names defensively.
    """

    vector: np.ndarray
    stages_ms: Mapping[str, float] = field(default_factory=dict)


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

    def embed_audio(self, raw_bytes: bytes) -> EmbedResult:
        """Return an `EmbedResult` whose vector has shape (EMBEDDING_DIM,)."""
        ...

    def embed_text(self, text: str) -> EmbedResult:
        """Return an `EmbedResult` whose vector has shape (EMBEDDING_DIM,)."""
        ...
