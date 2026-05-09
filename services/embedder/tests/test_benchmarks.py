"""Performance benchmarks for the stub embedder backend.

Cheap, runs everywhere, catches regressions in the zero-dependency
code path (SHA-256 + numpy PRNG). CLAP-backend benches live in a
separate file so they can be wholly skipped when the `clap` extra
isn't installed.

Run:

    uv run pytest -m benchmark
    uv run pytest -m benchmark --benchmark-only
    uv run pytest tests/test_benchmarks.py::test_stub_embed_audio[1mb]

Compare against a previous run (pytest-benchmark stores under
`.benchmarks/`):

    uv run pytest -m benchmark --benchmark-autosave
    uv run pytest -m benchmark --benchmark-compare

The `benchmark` fixture takes a callable; it auto-calibrates iteration
counts so overhead stays under ~5% of the measured op. Don't put any
non-bench logic inside the wrapped callable — it'll get attributed to
the timing.
"""

from __future__ import annotations

from collections.abc import Callable

import numpy as np
import pytest

from embedder.stub import StubEmbedder

pytestmark = pytest.mark.benchmark


@pytest.fixture
def stub() -> StubEmbedder:
    return StubEmbedder(model_version="stub-v1", loaded=True)


@pytest.mark.parametrize(
    "size_bytes",
    [
        1024,             # ~1 KB — short metadata-like input
        1024 * 1024,      # ~1 MB — typical small audio clip
        5 * 1024 * 1024,  # ~5 MB — longer clip
    ],
    ids=["1kb", "1mb", "5mb"],
)
def test_stub_embed_audio(
    benchmark: Callable[..., object],
    stub: StubEmbedder,
    size_bytes: int,
) -> None:
    # Random bytes hit the SHA-256 + PRNG path in full; zero-filled
    # bytes would compress in CPU caches and underestimate cost.
    rng = np.random.default_rng(seed=0xC0FFEE)
    payload = rng.bytes(size_bytes)
    result = benchmark(stub.embed_audio, payload)
    # Sanity: the bench callable still returned a real EmbedResult so
    # we know we measured the real path, not a cached short-circuit.
    assert result.vector.shape == (512,)


@pytest.mark.parametrize(
    "text_len",
    [16, 256, 4096],
    ids=["short", "medium", "long"],
)
def test_stub_embed_text(
    benchmark: Callable[..., object],
    stub: StubEmbedder,
    text_len: int,
) -> None:
    text = "x" * text_len
    result = benchmark(stub.embed_text, text)
    assert result.vector.shape == (512,)
