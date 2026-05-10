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

        # Decode to mono float32. soundfile (libsndfile) is the fast path —
        # works for ~99% of MP3/FLAC/OGG/WAV. It can fail with
        # `LibsndfileError: Format not recognised` on tracks where the
        # truncated byte slice doesn't begin on a clean MPEG frame
        # boundary, or on files with off-by-more-than-1% Xing/LAME headers
        # (we see a stderr warning for those even when decode succeeds).
        # Fall back to librosa.load(), which routes through audioread →
        # ffmpeg and tolerates a much wider set of broken containers.
        t0 = time.perf_counter()
        audio, sr = _decode_audio(raw_bytes)
        if audio.ndim == 2:
            audio = audio.mean(axis=1)
        stages["decode"] = (time.perf_counter() - t0) * 1000.0
        del soundfile  # Imported only to surface ImportError early.

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


def _decode_audio(raw_bytes: bytes) -> tuple[np.ndarray, int]:
    """Decode encoded audio bytes to (samples, sample_rate).

    Tries soundfile first (libsndfile) and falls back to librosa.load on
    `LibsndfileError`. The fallback writes bytes to a NamedTemporaryFile
    because audioread (librosa's non-soundfile path) needs a real path —
    it shells out to ffmpeg, which doesn't read from BytesIO.
    """
    import soundfile  # type: ignore[import-not-found]

    try:
        with io.BytesIO(raw_bytes) as buf:
            audio, sr = soundfile.read(buf, dtype="float32", always_2d=False)
        return audio, int(sr)
    except soundfile.LibsndfileError as primary:
        logger.warning(
            "soundfile decode failed (%s); falling back to librosa/ffmpeg",
            primary,
        )
        return _decode_via_librosa(raw_bytes, primary)


def _decode_via_librosa(
    raw_bytes: bytes, primary_error: Exception
) -> tuple[np.ndarray, int]:
    """librosa.load via a temporary file. Re-raises with the original
    libsndfile error context so failures stay debuggable."""
    import os
    import tempfile

    import librosa  # type: ignore[import-not-found]

    fd, path = tempfile.mkstemp(suffix=".audio")
    os.close(fd)
    try:
        with open(path, "wb") as f:
            f.write(raw_bytes)
        # sr=None preserves the source rate so the resample step decides
        # the target. mono=True so we hand back a 1-D array — librosa's
        # mono fold uses the librosa convention. soundfile would have
        # returned (samples, channels) for stereo and the caller folds
        # via mean(axis=1); librosa's stereo shape is the transpose
        # (channels, samples), so we let librosa do the fold for us
        # instead of replicating that shape gymnastic here.
        audio, sr = librosa.load(path, sr=None, mono=True)
        return audio, int(sr)
    except Exception as fallback_err:
        raise RuntimeError(
            f"audio decode failed via both soundfile ({primary_error}) "
            f"and librosa fallback ({fallback_err})"
        ) from fallback_err
    finally:
        try:
            os.unlink(path)
        except OSError:
            pass
