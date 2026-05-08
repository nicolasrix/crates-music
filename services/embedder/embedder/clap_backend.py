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

import numpy as np

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
        self._model = laion_clap.CLAP_Module(enable_fusion=False)
        self._model.load_ckpt(checkpoint_path)
        self._model_version = model_version or _derive_version(checkpoint_path)
        self._loaded = True

    @property
    def model_version(self) -> str:
        return self._model_version

    @property
    def loaded(self) -> bool:
        return self._loaded

    def embed_audio(self, raw_bytes: bytes) -> np.ndarray:
        import soundfile  # type: ignore[import-not-found]
        import librosa  # type: ignore[import-not-found]

        # Decode to mono float32, then resample to 48 kHz.
        with io.BytesIO(raw_bytes) as buf:
            audio, sr = soundfile.read(buf, dtype="float32", always_2d=False)
        if audio.ndim == 2:
            audio = audio.mean(axis=1)
        if sr != self.SAMPLE_RATE:
            audio = librosa.resample(audio, orig_sr=sr, target_sr=self.SAMPLE_RATE)
        # CLAP expects (batch, samples).
        batch = audio[np.newaxis, :].astype(np.float32)
        emb = self._model.get_audio_embedding_from_data(x=batch, use_tensor=False)
        return _l2_normalize(np.asarray(emb[0], dtype=np.float32))

    def embed_text(self, text: str) -> np.ndarray:
        emb = self._model.get_text_embedding([text], use_tensor=False)
        return _l2_normalize(np.asarray(emb[0], dtype=np.float32))


def _derive_version(checkpoint_path: str) -> str:
    """Best-effort: take the checkpoint filename without extension."""
    import os

    base = os.path.basename(checkpoint_path)
    return base.rsplit(".", 1)[0] or "clap"


def _l2_normalize(v: np.ndarray) -> np.ndarray:
    norm = float(np.linalg.norm(v))
    return v / norm if norm > 0 else v
