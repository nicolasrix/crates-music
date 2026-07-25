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
) -> list[dict[str, Any]]:
    """Inject minimal fakes for laion_clap, soundfile, librosa, torch.

    Only the surface ClapEmbedder.__init__ touches needs to exist:
    - laion_clap.CLAP_Module(enable_fusion, amodel) → object with load_ckpt()
    - torch.load(...) → drives the weights_only-injection guard
    - torch.cuda.is_available() → bool (drives device selection)
    soundfile + librosa are imported but not called during construction.

    Returns a list that records every torch.load call's kwargs, so tests
    can assert the security guard injected `weights_only=True`.
    """
    load_calls: list[dict[str, Any]] = []

    fake_torch = types.ModuleType("torch")

    def fake_load(*_args: Any, **kwargs: Any) -> dict[str, Any]:
        load_calls.append(dict(kwargs))
        return {}

    fake_torch.load = fake_load  # type: ignore[attr-defined]
    fake_torch.cuda = types.SimpleNamespace(  # type: ignore[attr-defined]
        is_available=lambda: cuda_available
    )

    fake_laion = types.ModuleType("laion_clap")

    class FakeClapModule:
        def __init__(self, *, enable_fusion: bool = False, amodel: str = "") -> None:
            self.enable_fusion = enable_fusion
            self.amodel = amodel

        def load_ckpt(self, path: str) -> None:
            # Mirror laion_clap: this is where torch.load fires. Going
            # through the patched torch.load exercises the guard.
            import torch

            torch.load(path)
            self.checkpoint_path = path

    fake_laion.CLAP_Module = FakeClapModule  # type: ignore[attr-defined]

    fake_soundfile = types.ModuleType("soundfile")
    fake_librosa = types.ModuleType("librosa")

    monkeypatch.setitem(sys.modules, "laion_clap", fake_laion)
    monkeypatch.setitem(sys.modules, "soundfile", fake_soundfile)
    monkeypatch.setitem(sys.modules, "librosa", fake_librosa)
    monkeypatch.setitem(sys.modules, "torch", fake_torch)
    # Force a clean import so any cached state is rebuilt against the fakes.
    monkeypatch.delitem(sys.modules, "embedder.clap_backend", raising=False)
    return load_calls


def _import_clap_embedder() -> Any:
    from embedder.clap_backend import ClapEmbedder

    return ClapEmbedder


def _ckpt(tmp_path: Any, name: str = "model.pt", data: bytes = b"fake-weights") -> str:
    """Write a real on-disk checkpoint file so _verify_checkpoint passes."""
    p = tmp_path / name
    p.write_bytes(data)
    return str(p)


def test_clap_embedder_reports_cpu_when_cuda_unavailable(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _install_fake_modules(monkeypatch, cuda_available=False)
    ClapEmbedder = _import_clap_embedder()
    emb = ClapEmbedder(checkpoint_path=_ckpt(tmp_path))
    assert emb.device == "cpu"


def test_clap_embedder_reports_cuda_when_available(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _install_fake_modules(monkeypatch, cuda_available=True)
    ClapEmbedder = _import_clap_embedder()
    emb = ClapEmbedder(checkpoint_path=_ckpt(tmp_path))
    # On ROCm-built torch, HIP devices identify as "cuda" — that is the
    # contract laion_clap relies on, and what we surface upstream.
    assert emb.device == "cuda"


def test_clap_embedder_loaded_flag_is_true_after_construction(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _install_fake_modules(monkeypatch, cuda_available=False)
    ClapEmbedder = _import_clap_embedder()
    emb = ClapEmbedder(checkpoint_path=_ckpt(tmp_path))
    assert emb.loaded is True


def test_clap_embedder_derives_version_from_filename(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _install_fake_modules(monkeypatch, cuda_available=False)
    ClapEmbedder = _import_clap_embedder()
    emb = ClapEmbedder(
        checkpoint_path=_ckpt(tmp_path, "music_audioset_epoch_15_esc_90.14.pt")
    )
    assert emb.model_version == "music_audioset_epoch_15_esc_90.14"


def test_clap_embedder_explicit_version_overrides_filename(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _install_fake_modules(monkeypatch, cuda_available=False)
    ClapEmbedder = _import_clap_embedder()
    emb = ClapEmbedder(
        checkpoint_path=_ckpt(tmp_path, "whatever.pt"),
        model_version="clap-music-htsat-base",
    )
    assert emb.model_version == "clap-music-htsat-base"


# --- checkpoint hardening (security review #12) ---------------------------


def test_load_forces_weights_only_true(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    """The load path must pass weights_only=True to torch.load so a
    hostile checkpoint can't execute pickled code on load."""
    load_calls = _install_fake_modules(monkeypatch, cuda_available=False)
    ClapEmbedder = _import_clap_embedder()
    ClapEmbedder(checkpoint_path=_ckpt(tmp_path))
    assert load_calls, "torch.load should have been called during load_ckpt"
    assert load_calls[0].get("weights_only") is True


def test_missing_checkpoint_file_is_rejected(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    _install_fake_modules(monkeypatch, cuda_available=False)
    ClapEmbedder = _import_clap_embedder()
    with pytest.raises(RuntimeError, match="missing or not a regular file"):
        ClapEmbedder(checkpoint_path=str(tmp_path / "does-not-exist.pt"))


def test_sha256_pin_matches_loads_fine(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    import hashlib

    data = b"the-one-true-checkpoint"
    digest = hashlib.sha256(data).hexdigest()
    monkeypatch.setenv("CLAP_CHECKPOINT_SHA256", digest)
    _install_fake_modules(monkeypatch, cuda_available=False)
    ClapEmbedder = _import_clap_embedder()
    emb = ClapEmbedder(checkpoint_path=_ckpt(tmp_path, "pinned.pt", data))
    assert emb.loaded is True


def test_sha256_pin_mismatch_refuses_to_load(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    monkeypatch.setenv("CLAP_CHECKPOINT_SHA256", "0" * 64)
    _install_fake_modules(monkeypatch, cuda_available=False)
    ClapEmbedder = _import_clap_embedder()
    with pytest.raises(RuntimeError, match="sha256 mismatch"):
        ClapEmbedder(checkpoint_path=_ckpt(tmp_path, "tampered.pt", b"not-the-pinned-bytes"))
