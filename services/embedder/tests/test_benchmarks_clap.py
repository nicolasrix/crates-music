"""Performance benchmarks for the CLAP backend (real model inference).

Module-level `importorskip` collects the whole file as skipped when
the `clap` extra isn't installed. A second guard checks for an
on-disk checkpoint via `CLAP_CHECKPOINT`; without one, the benches
skip individually with a clear reason.

To enable:

    uv sync --extra clap
    export CLAP_CHECKPOINT=/path/to/clap.pt
    uv run pytest -m benchmark tests/test_benchmarks_clap.py

A real model checkpoint produces meaningless embeddings on a synthetic
sine wave, but timing-wise the cost is identical to a real clip — the
decoder + resampler + forward pass all see a real signal. Generating
audio in-fixture (rather than shipping a WAV file) keeps the repo
small and lets us parameterize over clip length cleanly.
"""

from __future__ import annotations

import io
import os
from collections.abc import Callable

import numpy as np
import pytest

# Both required: laion-clap brings the model, soundfile brings the
# in-memory WAV writer. Skip the whole module if either is missing —
# the gateway can't run CLAP without both anyway.
laion_clap = pytest.importorskip(
    "laion_clap", reason="install with: uv sync --extra clap"
)
soundfile = pytest.importorskip(
    "soundfile", reason="bundled with the clap extra"
)

pytestmark = pytest.mark.benchmark

CLAP_CHECKPOINT = os.environ.get("CLAP_CHECKPOINT")
clap_checkpoint_required = pytest.mark.skipif(
    CLAP_CHECKPOINT is None or not os.path.exists(CLAP_CHECKPOINT or ""),
    reason="set CLAP_CHECKPOINT to an on-disk checkpoint to enable CLAP benches",
)


@pytest.fixture(scope="module")
def clap_embedder():
    """Loaded once per pytest invocation — checkpoint load is multi-second."""
    from embedder.clap_backend import ClapEmbedder

    assert CLAP_CHECKPOINT is not None  # narrowed by the skipif on each test
    return ClapEmbedder(checkpoint_path=CLAP_CHECKPOINT)


def _synthetic_wav(seconds: float, sample_rate: int = 48_000) -> bytes:
    n_samples = int(seconds * sample_rate)
    t = np.linspace(0, seconds, n_samples, endpoint=False, dtype=np.float32)
    audio = (0.1 * np.sin(2.0 * np.pi * 440.0 * t)).astype(np.float32)
    buf = io.BytesIO()
    soundfile.write(buf, audio, sample_rate, format="WAV", subtype="PCM_16")
    return buf.getvalue()


@clap_checkpoint_required
@pytest.mark.parametrize(
    "seconds",
    [5.0, 30.0],
    ids=["5s", "30s"],
)
def test_clap_embed_audio(
    benchmark: Callable[..., object],
    clap_embedder,
    seconds: float,
) -> None:
    payload = _synthetic_wav(seconds)
    # CLAP forward passes are seconds-scale on CPU and ~hundreds-of-ms
    # on GPU, so the default ~10 rounds × 5 iterations would take
    # minutes per case. Pedantic mode lets us cap the budget.
    benchmark.pedantic(
        clap_embedder.embed_audio,
        args=(payload,),
        rounds=5,
        iterations=1,
        warmup_rounds=1,
    )


@clap_checkpoint_required
@pytest.mark.parametrize(
    "text_len",
    [16, 256, 4096],
    ids=["short", "medium", "long"],
)
def test_clap_embed_text(
    benchmark: Callable[..., object],
    clap_embedder,
    text_len: int,
) -> None:
    text = ("lorem ipsum " * (text_len // 12 + 1))[:text_len]
    benchmark.pedantic(
        clap_embedder.embed_text,
        args=(text,),
        rounds=5,
        iterations=1,
        warmup_rounds=1,
    )
