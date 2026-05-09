"""Unit tests for ClapEmbedder's lifecycle without real CLAP/torch.

The CI / dev install does not pull `torch` or `laion-clap` (those are
behind the `[clap]` extra). We mock the imports at sys.modules so we
can exercise the construction + device-reporting logic without the
multi-GB dependency tree. Real CLAP behavior is verified by the
end-to-end smoke run, not unit tests.
"""

from __future__ import annotations

import sys
import types
from typing import Any

import pytest


def _install_fake_modules(
    monkeypatch: pytest.MonkeyPatch, *, cuda_available: bool
) -> None:
    """Inject minimal fakes for laion_clap, soundfile, librosa, torch.

    Only the surface ClapEmbedder.__init__ touches needs to exist:
    - laion_clap.CLAP_Module(enable_fusion, amodel) → object with load_ckpt()
    - torch.cuda.is_available() → bool (drives device selection)
    soundfile + librosa are imported but not called during construction.
    """
    fake_laion = types.ModuleType("laion_clap")

    class FakeClapModule:
        def __init__(self, *, enable_fusion: bool = False, amodel: str = "") -> None:
            self.enable_fusion = enable_fusion
            self.amodel = amodel

        def load_ckpt(self, path: str) -> None:
            self.checkpoint_path = path

    fake_laion.CLAP_Module = FakeClapModule  # type: ignore[attr-defined]

    fake_soundfile = types.ModuleType("soundfile")
    fake_librosa = types.ModuleType("librosa")

    fake_torch = types.ModuleType("torch")
    fake_torch.cuda = types.SimpleNamespace(  # type: ignore[attr-defined]
        is_available=lambda: cuda_available
    )

    monkeypatch.setitem(sys.modules, "laion_clap", fake_laion)
    monkeypatch.setitem(sys.modules, "soundfile", fake_soundfile)
    monkeypatch.setitem(sys.modules, "librosa", fake_librosa)
    monkeypatch.setitem(sys.modules, "torch", fake_torch)
    # Force a clean import so any cached state is rebuilt against the fakes.
    monkeypatch.delitem(sys.modules, "embedder.clap_backend", raising=False)


def _import_clap_embedder() -> Any:
    from embedder.clap_backend import ClapEmbedder

    return ClapEmbedder


def test_clap_embedder_reports_cpu_when_cuda_unavailable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _install_fake_modules(monkeypatch, cuda_available=False)
    ClapEmbedder = _import_clap_embedder()
    emb = ClapEmbedder(checkpoint_path="/fake/path/model.pt")
    assert emb.device == "cpu"


def test_clap_embedder_reports_cuda_when_available(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _install_fake_modules(monkeypatch, cuda_available=True)
    ClapEmbedder = _import_clap_embedder()
    emb = ClapEmbedder(checkpoint_path="/fake/path/model.pt")
    # On ROCm-built torch, HIP devices identify as "cuda" — that is the
    # contract laion_clap relies on, and what we surface upstream.
    assert emb.device == "cuda"


def test_clap_embedder_loaded_flag_is_true_after_construction(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _install_fake_modules(monkeypatch, cuda_available=False)
    ClapEmbedder = _import_clap_embedder()
    emb = ClapEmbedder(checkpoint_path="/fake/path/model.pt")
    assert emb.loaded is True


def test_clap_embedder_derives_version_from_filename(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _install_fake_modules(monkeypatch, cuda_available=False)
    ClapEmbedder = _import_clap_embedder()
    emb = ClapEmbedder(
        checkpoint_path="/models/music_audioset_epoch_15_esc_90.14.pt"
    )
    assert emb.model_version == "music_audioset_epoch_15_esc_90.14"


def test_clap_embedder_explicit_version_overrides_filename(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _install_fake_modules(monkeypatch, cuda_available=False)
    ClapEmbedder = _import_clap_embedder()
    emb = ClapEmbedder(
        checkpoint_path="/models/whatever.pt",
        model_version="clap-music-htsat-base",
    )
    assert emb.model_version == "clap-music-htsat-base"
