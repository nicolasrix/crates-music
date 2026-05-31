"""Deterministic stub embedder.

Two purposes:
1. Tests — exercises the FastAPI surface without loading multi-GB weights.
2. Degraded mode — if production starts the service before the real
   weights are available, the stub keeps the API alive so the gateway
   can boot in degraded mode rather than crash-looping.

The stub hashes input bytes with SHA-256, expands the digest into a
`dim`-dim float vector with a deterministic PRNG seeded by the hash, and
L2-normalizes. Output is stable across runs. The `dim` is configurable so
the stub can stand in for either CLAP (512) or CLaMP 3 (768) in tests.
"""

from __future__ import annotations

import hashlib
import time

import numpy as np

from embedder.protocol import EmbedResult

DEFAULT_DIM: int = 512


class StubEmbedder:
    """Deterministic, weights-free embedder for tests + degraded mode."""

    def __init__(
        self,
        model_version: str = "stub-v1",
        loaded: bool = True,
        dim: int = DEFAULT_DIM,
    ) -> None:
        if dim <= 0:
            raise ValueError(f"dim must be positive, got {dim}")
        self._model_version = model_version
        self._loaded = loaded
        self._dim = dim

    @property
    def model_version(self) -> str:
        return self._model_version

    @property
    def loaded(self) -> bool:
        return self._loaded

    @property
    def device(self) -> str:
        return "cpu"

    @property
    def dim(self) -> int:
        return self._dim

    def embed_audio(self, raw_bytes: bytes) -> EmbedResult:
        return self._timed_hash(b"audio:" + raw_bytes)

    def embed_text(self, text: str) -> EmbedResult:
        return self._timed_hash(b"text:" + text.encode("utf-8"))

    def _timed_hash(self, payload: bytes) -> EmbedResult:
        # Single "hash" stage — the stub doesn't decode or run a model,
        # so reporting decode/resample/gpu_forward would be a lie. The
        # stage name is still useful to the gateway: it confirms the
        # sidecar emitted timings without claiming work it didn't do.
        t0 = time.perf_counter()
        vec = self._hash_to_vector(payload)
        elapsed_ms = (time.perf_counter() - t0) * 1000.0
        return EmbedResult(vector=vec, stages_ms={"hash": elapsed_ms})

    def _hash_to_vector(self, payload: bytes) -> np.ndarray:
        # Seed numpy's PRNG with the hash so different inputs produce
        # different vectors and the same input is bit-stable.
        digest = hashlib.sha256(payload).digest()
        seed = int.from_bytes(digest[:8], "little")
        rng = np.random.default_rng(seed)
        v = rng.standard_normal(self._dim).astype(np.float32)
        # L2-normalize so cosine similarity ≈ dot product, matching CLAP.
        norm = float(np.linalg.norm(v))
        if norm > 0:
            v /= norm
        return v
