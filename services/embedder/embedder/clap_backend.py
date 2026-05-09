"""CLAP backend — production embedder.

Requires the `clap` optional extra:
    uv pip install -e '.[clap]'

This module is imported lazily by `app._default_embedder()` so the
test/dev install (no torch, no laion-clap) doesn't pay the import cost
or fail with ModuleNotFoundError on every startup.

The model produces L2-normalized 512-dim float vectors for both audio
and text, which share the same embedding space. Audio bytes are
expected to be a decodable container (mp3/flac/ogg/wav); decoding is
done with soundfile + librosa.resample to CLAP's expected 48 kHz mono.
"""

from __future__ import annotations

import io
import logging
import time

import numpy as np

from embedder.protocol import EmbedResult

logger = logging.getLogger(__name__)


class ClapEmbedder:
    """Wraps a LAION CLAP checkpoint.

    Loaded eagerly at construction. If the checkpoint is missing or the
    `clap` extra isn't installed, this raises with a clear message —
    callers can catch and fall back to `StubEmbedder` for degraded mode.
    """

    SAMPLE_RATE = 48000  # CLAP expects 48 kHz mono.

    def __init__(self, checkpoint_path: str, model_version: str | None = None) -> None:
        try:
            import laion_clap  # type: ignore[import-not-found]
            import soundfile  # type: ignore[import-not-found]  # noqa: F401
            import librosa  # type: ignore[import-not-found]  # noqa: F401
        except ImportError as e:
            raise RuntimeError(
                "CLAP backend requires the [clap] extra: "
                "`uv pip install -e '.[clap]'`. "
                f"Underlying import error: {e}"
            ) from e

        logger.info("loading CLAP checkpoint from %s", checkpoint_path)
        self._model = laion_clap.CLAP_Module(enable_fusion=False, amodel="HTSAT-base")
        self._model.load_ckpt(checkpoint_path)
        self._model_version = model_version or _derive_version(checkpoint_path)
        self._loaded = True
        self._device = _detect_device()
        logger.info("CLAP loaded on device=%s", self._device)

    @property
    def model_version(self) -> str:
        return self._model_version

    @property
    def loaded(self) -> bool:
        return self._loaded

    @property
    def device(self) -> str:
        return self._device

    def embed_audio(self, raw_bytes: bytes) -> EmbedResult:
        import soundfile  # type: ignore[import-not-found]
        import librosa  # type: ignore[import-not-found]

        stages: dict[str, float] = {}

        # Decode to mono float32.
        t0 = time.perf_counter()
        with io.BytesIO(raw_bytes) as buf:
            audio, sr = soundfile.read(buf, dtype="float32", always_2d=False)
        if audio.ndim == 2:
            audio = audio.mean(axis=1)
        stages["decode"] = (time.perf_counter() - t0) * 1000.0

        # Resample to 48 kHz if needed.
        t0 = time.perf_counter()
        if sr != self.SAMPLE_RATE:
            audio = librosa.resample(audio, orig_sr=sr, target_sr=self.SAMPLE_RATE)
        stages["resample"] = (time.perf_counter() - t0) * 1000.0

        # GPU forward pass. (CPU when CUDA isn't available — same code path.)
        t0 = time.perf_counter()
        batch = audio[np.newaxis, :].astype(np.float32)
        emb = self._model.get_audio_embedding_from_data(x=batch, use_tensor=False)
        vec = _l2_normalize(np.asarray(emb[0], dtype=np.float32))
        stages["gpu_forward"] = (time.perf_counter() - t0) * 1000.0

        return EmbedResult(vector=vec, stages_ms=stages)

    def embed_text(self, text: str) -> EmbedResult:
        t0 = time.perf_counter()
        emb = self._model.get_text_embedding([text], use_tensor=False)
        vec = _l2_normalize(np.asarray(emb[0], dtype=np.float32))
        elapsed_ms = (time.perf_counter() - t0) * 1000.0
        return EmbedResult(vector=vec, stages_ms={"gpu_forward": elapsed_ms})


def _derive_version(checkpoint_path: str) -> str:
    """Best-effort: take the checkpoint filename without extension."""
    import os

    base = os.path.basename(checkpoint_path)
    return base.rsplit(".", 1)[0] or "clap"


def _detect_device() -> str:
    """Return "cuda" if a HIP/CUDA torch device is usable, else "cpu".

    laion_clap auto-moves the model to GPU when torch.cuda.is_available()
    is True — including on ROCm-built torch where HIP devices report as
    "cuda". So this single check matches what the model is actually
    doing without us having to introspect model parameters (which would
    require touching CLAP's internals).
    """
    try:
        import torch

        return "cuda" if torch.cuda.is_available() else "cpu"
    except ImportError:
        return "cpu"


def _l2_normalize(v: np.ndarray) -> np.ndarray:
    norm = float(np.linalg.norm(v))
    return v / norm if norm > 0 else v
