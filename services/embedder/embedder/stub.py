"""Deterministic stub embedder.

Two purposes:
1. Tests — exercises the FastAPI surface without loading multi-GB weights.
2. Degraded mode — if production starts the service before the CLAP
   weights are available, the stub keeps the API alive so the gateway
   can boot in degraded mode rather than crash-looping.

The stub hashes input bytes with SHA-256, expands the digest into a
512-dim float vector with a deterministic PRNG seeded by the hash, and
L2-normalizes. Output is stable across runs.
"""

from __future__ import annotations

import hashlib

import numpy as np


class StubEmbedder:
    """Deterministic, weights-free embedder for tests + degraded mode."""

    def __init__(self, model_version: str = "stub-v1", loaded: bool = True) -> None:
        self._model_version = model_version
        self._loaded = loaded

    @property
    def model_version(self) -> str:
        return self._model_version

    @property
    def loaded(self) -> bool:
        return self._loaded

    def embed_audio(self, raw_bytes: bytes) -> np.ndarray:
        return self._hash_to_vector(b"audio:" + raw_bytes)

    def embed_text(self, text: str) -> np.ndarray:
        return self._hash_to_vector(b"text:" + text.encode("utf-8"))

    @staticmethod
    def _hash_to_vector(payload: bytes) -> np.ndarray:
        # Seed numpy's PRNG with the hash so different inputs produce
        # different vectors and the same input is bit-stable.
        digest = hashlib.sha256(payload).digest()
        seed = int.from_bytes(digest[:8], "little")
        rng = np.random.default_rng(seed)
        from embedder.app import EMBEDDING_DIM

        v = rng.standard_normal(EMBEDDING_DIM).astype(np.float32)
        # L2-normalize so cosine similarity ≈ dot product, matching CLAP.
        norm = float(np.linalg.norm(v))
        if norm > 0:
            v /= norm
        return v
