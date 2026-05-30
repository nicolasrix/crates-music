"""Unit tests for Clamp3Embedder's pure surface, without real torch/MERT.

The dev / CI install does not pull `torch`, `torchaudio`, `transformers`
or `nnAudio` (those live behind the `[clamp3]` extra). Everything heavy
is imported lazily *inside* `Clamp3Embedder.__init__` / `embed_audio`,
so the module imports fine here and we can exercise:

  - the class-level wire contract (DIM / SAMPLE_RATE),
  - `_derive_version` (pure string logic),
  - `_detect_device` (light torch fake),
  - the "extra not installed" failure path,
  - `embed_text`'s deliberate NotImplementedError.

Real inference is verified by the end-to-end smoke run + the
`test_benchmarks_clap.py`-style opt-in suite, not by unit tests.
"""

from __future__ import annotations

import sys
import types

import pytest

from embedder.clamp3_backend import Clamp3Embedder, _derive_version, _detect_device


# --- wire contract ---------------------------------------------------------
#
# These constants are load-bearing: the gateway's ANN index has a fixed
# dimensionality and MERT only accepts 24 kHz mono. A silent change here
# would corrupt every embedding written after the change, so pin them.


def test_dim_is_768() -> None:
    assert Clamp3Embedder.DIM == 768


def test_sample_rate_is_24khz() -> None:
    assert Clamp3Embedder.SAMPLE_RATE == 24000


# --- _derive_version -------------------------------------------------------


def test_derive_version_keeps_descriptive_saas_filename() -> None:
    # The saas checkpoint encodes every hyperparameter in its name; the
    # gateway content-addresses embeddings by (track_id, model_version),
    # so we keep the full stem rather than collapsing it.
    path = "/models/weights_clamp3_saas_h_size_768_t_model_length_128_p_length_512.pth"
    assert (
        _derive_version(path)
        == "weights_clamp3_saas_h_size_768_t_model_length_128_p_length_512"
    )


def test_derive_version_handles_no_extension() -> None:
    assert _derive_version("/models/clamp3") == "clamp3"


def test_derive_version_falls_back_when_stem_empty() -> None:
    # A pathological ".pth" basename has an empty stem — fall back to the
    # literal "clamp3" rather than store an empty model_version.
    assert _derive_version("/models/.pth") == "clamp3"


# --- _detect_device --------------------------------------------------------


def _fake_torch(*, cuda_available: bool) -> types.ModuleType:
    fake = types.ModuleType("torch")
    fake.cuda = types.SimpleNamespace(  # type: ignore[attr-defined]
        is_available=lambda: cuda_available
    )
    return fake


def test_detect_device_reports_cpu_when_cuda_unavailable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setitem(sys.modules, "torch", _fake_torch(cuda_available=False))
    assert _detect_device() == "cpu"


def test_detect_device_reports_cuda_when_available(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # On ROCm-built torch, HIP devices identify as "cuda" — that is the
    # convention surfaced via /healthz so the gateway can spot silent
    # CPU fallback after a ROCm install.
    monkeypatch.setitem(sys.modules, "torch", _fake_torch(cuda_available=True))
    assert _detect_device() == "cuda"


def test_detect_device_reports_cpu_when_torch_missing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # A None entry in sys.modules makes `import torch` raise ImportError
    # deterministically, regardless of whether torch is really installed.
    monkeypatch.setitem(sys.modules, "torch", None)
    assert _detect_device() == "cpu"


# --- construction failure path --------------------------------------------


def test_init_raises_clear_error_without_clamp3_extra(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # Force the lazy `import torch` inside __init__ to fail even if a dev
    # has the extra installed, so the assertion is environment-stable.
    monkeypatch.setitem(sys.modules, "torch", None)
    with pytest.raises(RuntimeError, match=r"\[clamp3\] extra"):
        Clamp3Embedder(checkpoint_path="/fake/ckpt.pth", mert_folder="/fake/mert")


# --- embed_text ------------------------------------------------------------


def test_embed_text_not_implemented() -> None:
    # Text encoding is vendored but deliberately not wired until P6.9
    # (text-query stations). It must fail loudly, not return garbage.
    # Build a bare instance so we don't trigger the heavy __init__.
    emb = object.__new__(Clamp3Embedder)
    with pytest.raises(NotImplementedError, match="text"):
        emb.embed_text("rainy sunday afternoon")
