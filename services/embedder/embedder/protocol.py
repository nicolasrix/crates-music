"""Embedder protocol: any backend that can embed raw audio bytes and text.

Three implementations:
- `ClapEmbedder` (gated behind the `clap` extra) — wraps LAION CLAP, 512-dim.
- `Clamp3Embedder` (gated behind the `clamp3` extra) — wraps CLaMP 3, 768-dim.
- `StubEmbedder` (tests + degraded-mode fallback) — deterministic hash-based,
  default 512-dim but the constructor takes a `dim` so the stub can stand in
  for either backend's wire shape during tests.

All backends produce L2-normalized float32 vectors of shape `(dim,)` plus a
per-stage timing breakdown in milliseconds, so the gateway can attribute time
inside `embed_audio` / `embed_text` to its sub-steps via the
`Server-Timing` HTTP response header. `dim` is a per-backend property so the
sidecar can be swapped without the FastAPI layer hardcoding a dimension.
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
    """Anything that can produce embedding vectors of a fixed dimension."""

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

    @property
    def dim(self) -> int:
        """Vector dimension this backend produces.

        Read by `/healthz` and by `_build_embed_response` instead of a
        hardcoded module constant. Keeps the FastAPI layer agnostic to
        the chosen backend (CLAP=512, CLaMP 3=768, stub=ctor-arg).
        """
        ...

    def embed_audio(self, raw_bytes: bytes) -> EmbedResult:
        """Return an `EmbedResult` whose vector has shape (self.dim,)."""
        ...

    def embed_text(self, text: str) -> EmbedResult:
        """Return an `EmbedResult` whose vector has shape (self.dim,)."""
        ...
