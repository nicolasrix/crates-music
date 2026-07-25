"""Tests for the vendored ffmpeg decode fallback in `_clamp3.audio_io`.

These cover the error-handling contract only — they monkeypatch
`subprocess.run` so they need neither ffmpeg installed nor a real hang.

`audio_io` eagerly imports `soundfile`/`torch`/`torchaudio` (the
`[clamp3]` extra), which the lean dev/CI install doesn't pull, so the
whole module skips there and runs wherever the extra is present (the
clamp3 image, the GPU box).
"""

import subprocess

import pytest

audio_io = pytest.importorskip("embedder._clamp3.audio_io")


def test_ffmpeg_timeout_becomes_runtime_error(monkeypatch):
    """A hanging ffmpeg (TimeoutExpired) surfaces as a RuntimeError that
    names the timeout, rather than propagating the raw subprocess error."""

    def fake_run(*_args, **kwargs):
        # Mirror what subprocess.run does on timeout: kill + raise.
        raise subprocess.TimeoutExpired(cmd="ffmpeg", timeout=kwargs.get("timeout", 0))

    monkeypatch.setattr(audio_io.subprocess, "run", fake_run)

    with pytest.raises(RuntimeError, match="ffmpeg decode timed out"):
        audio_io._decode_via_ffmpeg(b"\x00\x01\x02", target_sr=16_000, is_mono=True)


def test_ffmpeg_decode_failure_becomes_runtime_error(monkeypatch):
    """A non-zero ffmpeg exit (CalledProcessError) still surfaces as a
    RuntimeError carrying the ffmpeg stderr tail — unchanged by the
    timeout addition."""

    def fake_run(*_args, **_kwargs):
        raise subprocess.CalledProcessError(
            returncode=1, cmd="ffmpeg", stderr=b"Invalid data found when processing input"
        )

    monkeypatch.setattr(audio_io.subprocess, "run", fake_run)

    with pytest.raises(RuntimeError, match="ffmpeg decode failed: .*Invalid data"):
        audio_io._decode_via_ffmpeg(b"garbage", target_sr=16_000, is_mono=True)


def test_ffmpeg_run_is_passed_a_timeout(monkeypatch):
    """Regression guard: the decode call must always pass a bounded
    `timeout=` so ffmpeg can't hang unbounded."""
    seen = {}

    def fake_run(*_args, **kwargs):
        seen.update(kwargs)
        raise subprocess.CalledProcessError(returncode=1, cmd="ffmpeg", stderr=b"stop here")

    monkeypatch.setattr(audio_io.subprocess, "run", fake_run)

    with pytest.raises(RuntimeError):
        audio_io._decode_via_ffmpeg(b"x", target_sr=16_000, is_mono=True)

    assert seen.get("timeout") == audio_io.FFMPEG_DECODE_TIMEOUT_SECONDS
    assert seen["timeout"] > 0
