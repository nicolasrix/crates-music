"""
Vendored from CLaMP 3 (https://github.com/sanderwood/clamp3).
Upstream path:   preprocessing/audio/MERT_utils.py
Upstream commit: 9016d2b0c8d12d1aa79c2e0ab201e6822bdc83a8
License:         MIT (see ./LICENSES/MIT-LICENSE)

Local modifications:
- 2026-05-30: Replaced upstream `load_audio(file_path, ...)` (which used
  `torchaudio.load(file_path)`) with `load_audio_bytes(raw_bytes, ...)` that
  reads from an in-memory bytes buffer via `soundfile.read(BytesIO(raw_bytes))`.
  Reason: torchaudio 2.9 dropped its built-in backends in favor of
  `torchcodec`, which has ABI issues with PyTorch ROCm wheels + FFmpeg 8 on
  the deployment target. Also the sidecar receives raw bytes from the FastAPI
  endpoint, not a file path, so the BytesIO form is the natural shape.
- 2026-05-30: Dropped `chunk_audio` (unused by anything we vendor),
  `find_audios` (directory walking belongs to the CLI we replace), and the
  module-level imports they pulled in (`mido`, `argparse`).
- 2026-05-30: Added an ffmpeg decode fallback. libsndfile (via soundfile)
  only handles WAV/FLAC/OGG/Opus/(recent)MP3 — it raises "Format not
  recognised" on AAC/M4A/ALAC/WMA, which Navidrome serves for part of a
  real library. ffmpeg (already in the image) decodes ~everything, so on a
  LibsndfileError we shell out to it. ffmpeg also downmixes + resamples in
  the same pass, so the mono-fold / resampler below no-op on that path.
"""

import io
import random
import subprocess
import tempfile

import numpy as np
import soundfile as sf
import torch
import torchaudio

np.set_printoptions(precision=4, suppress=True)


def load_audio_bytes(
    raw_bytes,
    target_sr,
    is_mono=True,
    is_normalize=False,
    crop_to_length_in_sec=None,
    crop_to_length_in_sample_points=None,
    crop_randomly=False,
    pad=False,
    return_start=False,
    device=torch.device('cpu'),
):
    """Decode an audio file held in `raw_bytes` and resample to target_sr.

    Args:
        raw_bytes (bytes): raw encoded audio. libsndfile (FLAC/WAV/OGG/Opus/
            MP3) is the fast path; anything it rejects (AAC/M4A/ALAC/WMA/...)
            falls back to ffmpeg.
        target_sr (int): target sample rate. If the source rate differs,
            torchaudio's Resample transform runs on `device`.
        is_mono (bool): fold to mono via channel mean.
        is_normalize (bool): scale to peak ±1.
        crop_to_length_in_sec / crop_to_length_in_sample_points / crop_randomly
            / pad: see crop_audio().
        return_start (bool): also return the crop start index.
        device (torch.device): where to run the resampler.

    Returns:
        torch.Tensor of shape (1, n_sample). With return_start=True, returns
        (tensor, start_index).
    """
    try:
        data, sample_rate = sf.read(io.BytesIO(raw_bytes), dtype='float32', always_2d=True)
        # soundfile yields (n_samples, n_channels); torchaudio convention is
        # (n_channels, n_samples). Transpose to match what the rest of the
        # pipeline (and upstream MERT_utils.load_audio) expects.
        waveform = torch.from_numpy(data.T)
    except sf.LibsndfileError:
        # libsndfile can't parse the container (AAC/M4A/ALAC/WMA/...).
        # ffmpeg decodes it AND gives us mono @ target_sr in one pass, so
        # the mono-fold and resampler below become no-ops on this path.
        waveform = _decode_via_ffmpeg(raw_bytes, target_sr, is_mono)
        sample_rate = target_sr
    if waveform.shape[0] > 1:
        if is_mono:
            waveform = torch.mean(waveform, dim=0, keepdim=True)

    if is_normalize:
        waveform = waveform / waveform.abs().max()

    waveform, start = crop_audio(
        waveform,
        sample_rate,
        crop_to_length_in_sec=crop_to_length_in_sec,
        crop_to_length_in_sample_points=crop_to_length_in_sample_points,
        crop_randomly=crop_randomly,
        pad=pad,
    )

    if sample_rate != target_sr:
        resampler = torchaudio.transforms.Resample(sample_rate, target_sr)
        waveform = waveform.to(device)
        resampler = resampler.to(device)
        waveform = resampler(waveform)

    if return_start:
        return waveform, start
    return waveform


def _decode_via_ffmpeg(raw_bytes, target_sr, is_mono):
    """Decode arbitrary audio bytes via ffmpeg, returning a
    (n_channels, n_samples) float32 torch tensor already at `target_sr`.

    Fallback for formats libsndfile can't read (AAC/M4A/ALAC/WMA/...).
    ffmpeg ships in the image as a runtime dependency. We write to a temp
    file rather than piping to stdin so containers with trailing metadata
    (e.g. an M4A `moov` atom at EOF) stay seekable — a non-seekable pipe
    makes ffmpeg fail on exactly the formats we're here to rescue.

    ffmpeg downmixes (`-ac`) and resamples (`-ar`) for us, so the caller's
    mono-fold and torchaudio resampler are no-ops on this path.
    """
    channels = 1 if is_mono else 2
    with tempfile.NamedTemporaryFile(suffix=".audio") as tmp:
        tmp.write(raw_bytes)
        tmp.flush()
        try:
            proc = subprocess.run(
                [
                    "ffmpeg", "-nostdin", "-hide_banner", "-loglevel", "error",
                    "-i", tmp.name,
                    "-f", "f32le", "-acodec", "pcm_f32le",
                    "-ac", str(channels), "-ar", str(int(target_sr)),
                    "pipe:1",
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
            )
        except subprocess.CalledProcessError as e:
            # Truly undecodable even by ffmpeg — surface the ffmpeg stderr so
            # the failure reason in the queue is actionable, not just "500".
            detail = e.stderr.decode("utf-8", "replace").strip()[-500:]
            raise RuntimeError(f"ffmpeg decode failed: {detail}") from e
    # f32le is interleaved; reshape to (n_channels, n_samples). frombuffer is
    # read-only and .copy() makes it writable (torch warns on / mis-handles
    # non-writable arrays, and the downstream resampler may mutate in place).
    audio = np.frombuffer(proc.stdout, dtype=np.float32).reshape(-1, channels).T
    return torch.from_numpy(np.ascontiguousarray(audio).copy())


def crop_audio(
    waveform,
    sample_rate,
    crop_to_length_in_sec=None,
    crop_to_length_in_sample_points=None,
    crop_randomly=False,
    pad=False,
):
    """Crop waveform to specified length in seconds or sample points.
    Supports random cropping and padding.

    Args:
        waveform (torch.Tensor): waveform of shape (1, n_sample)
        sample_rate (int): sample rate of waveform
        crop_to_length_in_sec (float, optional): crop to specified length in seconds. Defaults to None.
        crop_to_length_in_sample_points (int, optional): crop to specified length in sample points. Defaults to None.
        crop_randomly (bool, optional): crop randomly. Defaults to False.
        pad (bool, optional): pad to specified length if waveform is shorter than specified length. Defaults to False.

    Returns:
        torch.Tensor: cropped waveform
        int: start index of cropped waveform in original waveform
    """
    assert crop_to_length_in_sec is None or crop_to_length_in_sample_points is None, \
    "Only one of crop_to_length_in_sec and crop_to_length_in_sample_points can be specified"

    # convert crop length to sample points
    crop_duration_in_sample = None
    if crop_to_length_in_sec:
        crop_duration_in_sample = int(sample_rate * crop_to_length_in_sec)
    elif crop_to_length_in_sample_points:
        crop_duration_in_sample = crop_to_length_in_sample_points

    # crop
    start = 0
    if crop_duration_in_sample:
        if waveform.shape[-1] > crop_duration_in_sample:
            if crop_randomly:
                start = random.randint(0, waveform.shape[-1] - crop_duration_in_sample)
            waveform = waveform[..., start:start + crop_duration_in_sample]

        elif waveform.shape[-1] < crop_duration_in_sample:
            if pad:
                waveform = torch.nn.functional.pad(waveform, (0, crop_duration_in_sample - waveform.shape[-1]))

    return waveform, start
