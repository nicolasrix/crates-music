"""CLaMP 3 backend — audio embedder built on the vendored `_clamp3` package.

Requires the `clamp3` optional extra:
    uv pip install -e '.[clamp3]'

The backend produces L2-normalized 768-dim float vectors for both audio
and text. They live in CLaMP 3's *shared* joint embedding space, so a
natural-language station query ("rainy sunday afternoon") and the stored
audio embeddings are directly comparable by cosine similarity in the same
content-ANN — no separate text index.

Audio pipeline (mirrors upstream `preprocessing/audio/extract_mert.py` +
`code/extract_clamp3.py`):

  raw bytes
    → soundfile decode (in `_clamp3.audio_io.load_audio_bytes`)
    → resample to 24 kHz mono (torchaudio inside load_audio_bytes)
    → MERT-v1-95M sliding 5-sec windows                       (n_chunks, 13, 768)
    → mean over 13 transformer layers                         (n_chunks, 768)
    → prepend / append zero markers (BOS / EOS)              (n_chunks+2, 768)
    → CLaMP 3 audio encoder (BERT, 12 layers, hidden=768)
    → avg-pool + audio_proj (global feature)                  (768,)
    → L2-normalize for cosine similarity

Text pipeline (mirrors upstream `code/extract_clamp3.py:100-178`, the
`.txt` branch):

  query string
    → split into non-empty lines, de-dup, join with the tokenizer's
      sep token
    → xlm-roberta-base tokenize                                (n_tokens,)
    → segment into MAX_TEXT_LENGTH (128)-token windows
    → CLaMP 3 text encoder per segment → global feature        (n_seg, 768)
    → token-count-weighted mean over segments                  (768,)
    → L2-normalize (same space as embed_audio)
"""

from __future__ import annotations

import logging
import threading
import time

import numpy as np

from embedder.protocol import EmbedResult

logger = logging.getLogger(__name__)


class Clamp3Embedder:
    """Wraps a CLaMP 3 unified checkpoint + MERT-v1-95M frontend.

    Loaded eagerly at construction. If either the checkpoint or the MERT
    folder is missing, or the `clamp3` extra isn't installed, this raises
    with a clear message — callers can catch and fall back to
    `StubEmbedder` for degraded mode.
    """

    SAMPLE_RATE = 24000  # MERT expects 24 kHz mono.
    DIM = 768  # CLaMP 3 saas joint-embedding dimension.
    SLIDING_WINDOW_SEC = 5  # MERT context length per chunk.
    SLIDING_OVERLAP_PCT = 0.0  # Non-overlapping; matches upstream default.

    def __init__(
        self,
        checkpoint_path: str,
        mert_folder: str,
        model_version: str | None = None,
    ) -> None:
        try:
            import torch  # type: ignore[import-not-found]  # noqa: F401
            import soundfile  # type: ignore[import-not-found]  # noqa: F401
            from transformers import BertConfig  # type: ignore[import-not-found]  # noqa: F401
        except ImportError as e:
            raise RuntimeError(
                "CLaMP 3 backend requires the [clamp3] extra: "
                "`uv pip install -e '.[clamp3]'`. "
                f"Underlying import error: {e}"
            ) from e

        import torch
        from transformers import AutoTokenizer, BertConfig

        from embedder._clamp3 import CLaMP3Model, HuBERTFeature
        from embedder._clamp3.config import (
            AUDIO_HIDDEN_SIZE,
            AUDIO_NUM_LAYERS,
            CLAMP3_HIDDEN_SIZE,
            M3_HIDDEN_SIZE,
            MAX_AUDIO_LENGTH,
            MAX_TEXT_LENGTH,
            PATCH_LENGTH,
            PATCH_NUM_LAYERS,
            TEXT_MODEL_NAME,
        )

        self._device = _detect_device()
        self._max_audio_length = MAX_AUDIO_LENGTH

        # BertConfigs match upstream extract_clamp3.py:42-53 exactly —
        # any drift here would mean load_state_dict mismatches and
        # silently broken inference.
        audio_config = BertConfig(
            vocab_size=1,
            hidden_size=AUDIO_HIDDEN_SIZE,
            num_hidden_layers=AUDIO_NUM_LAYERS,
            num_attention_heads=AUDIO_HIDDEN_SIZE // 64,
            intermediate_size=AUDIO_HIDDEN_SIZE * 4,
            max_position_embeddings=MAX_AUDIO_LENGTH,
        )
        symbolic_config = BertConfig(
            vocab_size=1,
            hidden_size=M3_HIDDEN_SIZE,
            num_hidden_layers=PATCH_NUM_LAYERS,
            num_attention_heads=M3_HIDDEN_SIZE // 64,
            intermediate_size=M3_HIDDEN_SIZE * 4,
            max_position_embeddings=PATCH_LENGTH,
        )

        logger.info("loading CLaMP 3 checkpoint from %s", checkpoint_path)
        model = CLaMP3Model(
            audio_config=audio_config,
            symbolic_config=symbolic_config,
            text_model_name=TEXT_MODEL_NAME,
            hidden_size=CLAMP3_HIDDEN_SIZE,
            load_m3=False,
        )
        # `weights_only=True` because the checkpoint is a trusted artefact
        # but the safer default rules out arbitrary-pickle code execution.
        ckpt = torch.load(checkpoint_path, map_location="cpu", weights_only=True)
        missing, unexpected = model.load_state_dict(ckpt["model"], strict=False)
        if missing or unexpected:
            # Mirror the Phase A verification: the slim vendored model
            # must align bit-for-bit with the saas checkpoint. Any drift
            # is a Defcon-1 signal (e.g. someone edited model.py and
            # removed a layer) — bail loudly rather than serve garbage.
            raise RuntimeError(
                f"CLaMP 3 state_dict mismatch: missing={missing}, "
                f"unexpected={unexpected}. Vendored model is out of sync "
                "with the checkpoint."
            )
        model = model.to(self._device).eval()
        self._model = model

        logger.info("loading MERT frontend from %s", mert_folder)
        mert = HuBERTFeature(
            mert_folder,
            self.SAMPLE_RATE,
            force_half=False,
            processor_normalize=True,
        )
        mert = mert.to(self._device)
        mert.eval()
        self._mert = mert

        # Text branch: xlm-roberta-base tokenizer feeds CLaMP 3's text
        # encoder. Loaded eagerly (same fail-fast contract as the model);
        # the clamp3 Dockerfiles prebake this into the HF cache so there's
        # no network hit at construction in production.
        logger.info("loading text tokenizer %s", TEXT_MODEL_NAME)
        self._tokenizer = AutoTokenizer.from_pretrained(TEXT_MODEL_NAME)
        self._max_text_length = MAX_TEXT_LENGTH

        # Fail loud on a degenerate tokenizer. xlm-roberta-base is a
        # SentencePiece model; if `sentencepiece` is missing or the vocab
        # files weren't baked into the (offline) HF cache, transformers
        # silently loads a 5-token vocab where every word maps to <unk> —
        # so "death metal" and "smooth jazz" embed identically and every
        # station query collapses. Audio is unaffected (no tokenizer), so
        # this hides unless explicitly checked. See the Dockerfile prebake
        # and the [clamp3] `sentencepiece` dependency.
        if self._tokenizer.vocab_size < 1000:
            raise RuntimeError(
                f"text tokenizer {TEXT_MODEL_NAME} loaded a degenerate vocab "
                f"(size={self._tokenizer.vocab_size}); the SentencePiece vocab "
                "is missing. Ensure `sentencepiece` is installed and the "
                "tokenizer files are in the HF cache."
            )
        if self._tokenizer("death metal")["input_ids"] == self._tokenizer(
            "smooth jazz"
        )["input_ids"]:
            raise RuntimeError(
                f"text tokenizer {TEXT_MODEL_NAME} maps distinct text to "
                "identical token ids — vocab not loaded (all <unk>). "
                "embed_text would return identical vectors for every query."
            )

        self._model_version = model_version or _derive_version(checkpoint_path)
        self._loaded = True
        # Serialize forward passes so the device's CUDA/HIP stream isn't
        # over-subscribed by concurrent FastAPI workers. asyncio.to_thread
        # in the handler still lets CPU decode + resample overlap.
        self._lock = threading.Lock()
        logger.info(
            "CLaMP 3 loaded on device=%s, version=%s",
            self._device,
            self._model_version,
        )

    @property
    def model_version(self) -> str:
        return self._model_version

    @property
    def loaded(self) -> bool:
        return self._loaded

    @property
    def device(self) -> str:
        return self._device

    @property
    def dim(self) -> int:
        return self.DIM

    def embed_audio(self, raw_bytes: bytes) -> EmbedResult:
        import torch

        from embedder._clamp3 import load_audio_bytes

        with self._lock:
            stages: dict[str, float] = {}

            t0 = time.perf_counter()
            waveform = load_audio_bytes(
                raw_bytes,
                target_sr=self.SAMPLE_RATE,
                is_mono=True,
                device=torch.device(self._device),
            )
            stages["decode"] = (time.perf_counter() - t0) * 1000.0

            with torch.no_grad():
                t0 = time.perf_counter()
                # process_wav returns (1, T) — confirmed empirically against
                # the upstream HuBERTFeature against m-a-p/MERT-v1-95M.
                wav = self._mert.process_wav(waveform).to(self._device)
                window = int(self.SAMPLE_RATE * self.SLIDING_WINDOW_SEC)
                stride = int(
                    self.SAMPLE_RATE
                    * self.SLIDING_WINDOW_SEC
                    * (1.0 - self.SLIDING_OVERLAP_PCT / 100.0)
                )
                chunks = [wav[:, i : i + window] for i in range(0, wav.shape[-1], stride)]
                # Upstream drops trailing chunks shorter than 1 sec — MERT's
                # positional conv struggles on very short inputs. Match exactly.
                if chunks and chunks[-1].shape[-1] < self.SAMPLE_RATE:
                    chunks = chunks[:-1]
                if not chunks:
                    raise RuntimeError(
                        f"audio too short for MERT (< {self.SLIDING_WINDOW_SEC + 1} sec)"
                    )
                # Each chunk forward: (L=13, B=1, H=768).
                features_per_chunk = [
                    self._mert(chunk, layer=None, reduction="mean") for chunk in chunks
                ]
                # Concat along the batch dim → (13, n_chunks, 768).
                features = torch.cat(features_per_chunk, dim=1)
                # Mean over the 13 transformer layers (upstream's
                # extract_mert.py --mean_features flag, which is mandatory
                # because extract_clamp3.py reshapes the saved .npy as
                # (N, 768)). Result: (n_chunks, 768).
                features = features.mean(dim=0)
                stages["mert"] = (time.perf_counter() - t0) * 1000.0

                t0 = time.perf_counter()
                # Prepend/append zero markers (BOS/EOS), matching upstream
                # extract_clamp3.py:122-123.
                zero = torch.zeros(
                    (1, self.DIM), device=self._device, dtype=features.dtype
                )
                audio_seq = torch.cat([zero, features, zero], dim=0)

                # Truncate to MAX_AUDIO_LENGTH and pad with zeros + mask.
                # For our gateway's ~2 min clips we never exceed 26 positions,
                # so this is a safety guard rather than a hot path. The
                # upstream multi-segment weight-merging code path is
                # unreachable under our input contract.
                seg = audio_seq[: self._max_audio_length]
                actual_len = seg.size(0)
                mask = torch.ones(
                    actual_len, device=self._device, dtype=torch.float32
                )
                if actual_len < self._max_audio_length:
                    pad_len = self._max_audio_length - actual_len
                    pad = torch.zeros(
                        (pad_len, self.DIM),
                        device=self._device,
                        dtype=features.dtype,
                    )
                    seg = torch.cat([seg, pad], dim=0)
                    mask = torch.cat(
                        [mask, torch.zeros(pad_len, device=self._device)], dim=0
                    )

                emb = self._model.get_audio_features(
                    audio_inputs=seg.unsqueeze(0),
                    audio_masks=mask.unsqueeze(0),
                    get_global=True,
                )  # (1, DIM)
                vec = emb.squeeze(0)
                norm = vec.norm()
                if norm > 0:
                    vec = vec / norm
                stages["clamp3"] = (time.perf_counter() - t0) * 1000.0

                arr = vec.detach().cpu().numpy().astype(np.float32)
            return EmbedResult(vector=arr, stages_ms=stages)

    def embed_text(self, text: str) -> EmbedResult:
        import torch

        with self._lock:
            stages: dict[str, float] = {}

            t0 = time.perf_counter()
            item = _join_text_lines(text, self._tokenizer.sep_token)
            # xlm-roberta wraps the string in <s> … </s> automatically;
            # we feed those special tokens through unchanged, exactly as
            # upstream's tokenizer(item) does.
            input_ids = self._tokenizer(item, return_tensors="pt")["input_ids"].squeeze(0)
            stages["tokenize"] = (time.perf_counter() - t0) * 1000.0

            with torch.no_grad():
                t0 = time.perf_counter()
                vec = self._encode_text_global(input_ids)
                norm = vec.norm()
                if norm > 0:
                    vec = vec / norm
                arr = vec.detach().cpu().numpy().astype(np.float32)
                stages["clamp3"] = (time.perf_counter() - t0) * 1000.0

            return EmbedResult(vector=arr, stages_ms=stages)

    def _encode_text_global(self, input_ids):
        """Token ids → a single global text feature (DIM,).

        Mirrors `extract_clamp3.py`'s `.txt` + `get_global=True` path:
        slice the token sequence into MAX_TEXT_LENGTH windows (the last
        window is the trailing slice, so it stays full-width when the
        text overruns a single window), encode each, then take a mean
        weighted by each window's real-token count. For a typical
        ≤500-char station prompt this is a single window and the weight
        cancels — the multi-window branch is the safety path for long
        prompts, kept faithful so embeddings match the offline extractor.
        """
        import torch

        max_len = self._max_text_length
        pad_id = self._tokenizer.pad_token_id
        n = int(input_ids.size(0))

        segments = [input_ids[i : i + max_len] for i in range(0, n, max_len)]
        # Last segment is the trailing window (matches upstream
        # `segment_list[-1] = input_data[-max_input_length:]`).
        segments[-1] = input_ids[-max_len:]

        features = []
        for seg in segments:
            seg_len = int(seg.size(0))
            mask = torch.cat(
                [torch.ones(seg_len), torch.zeros(max_len - seg_len)], dim=0
            )
            pad = torch.ones(max_len - seg_len, dtype=torch.long) * pad_id
            seg = torch.cat([seg, pad], dim=0)
            g = self._model.get_text_features(
                text_inputs=seg.unsqueeze(0).to(self._device),
                text_masks=mask.unsqueeze(0).to(self._device),
                get_global=True,
            )  # (1, DIM)
            features.append(g)

        full = n // max_len
        remain = n % max_len
        if remain == 0:
            weights = [max_len] * full
        else:
            weights = [max_len] * full + [remain]
        weight_t = torch.tensor(
            weights, device=self._device, dtype=torch.float32
        ).view(-1, 1)

        stacked = torch.cat(features, dim=0)  # (n_seg, DIM)
        return (stacked * weight_t).sum(dim=0) / weight_t.sum()


def _join_text_lines(text: str, sep_token: str) -> str:
    """Clean a query string the way upstream's `.txt` branch does.

    Upstream (`extract_clamp3.py:106-110`) splits on newlines, drops
    empties, de-dups, and joins the lines with the tokenizer's sep token.
    We keep that shape but de-dup *order-preservingly* (`dict.fromkeys`)
    instead of `list(set(...))`: a station query is a hot path, and
    `set` iteration order is per-process randomized for str, which would
    make the same multi-line query embed differently across embedder
    restarts. For a single-line prompt this is a no-op.
    """
    lines = [line for line in text.split("\n") if len(line) > 0]
    lines = list(dict.fromkeys(lines))
    return sep_token.join(lines)


def _derive_version(checkpoint_path: str) -> str:
    """Best-effort: take the checkpoint filename without extension.

    Mirrors `ClapEmbedder._derive_version`. The upstream CLaMP 3 saas
    weights have very long descriptive filenames
    (`weights_clamp3_saas_h_size_768_..._p_length_512.pth`) — keeping the
    full filename means model_version embeds every relevant hyperparameter
    in one string, which matches the embedding cache's content-addressing
    expectations on the gateway side.
    """
    import os

    base = os.path.basename(checkpoint_path)
    return base.rsplit(".", 1)[0] or "clamp3"


def _detect_device() -> str:
    """Return "cuda" if a HIP/CUDA torch device is usable, else "cpu".

    On ROCm-built torch (e.g. the rocm6.4 wheel index) HIP devices report
    as "cuda" — same convention CLAP uses; surfaced via /healthz so the
    gateway boot probe can detect silent CPU fallback after a ROCm install.
    """
    try:
        import torch

        return "cuda" if torch.cuda.is_available() else "cpu"
    except ImportError:
        return "cpu"
