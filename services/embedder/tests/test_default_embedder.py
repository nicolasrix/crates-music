"""Tests for `_default_embedder` env-var backend selection.

`_default_embedder` is the only place that reads `EMBEDDER_BACKEND` and
the per-backend env knobs. These tests pin the stub-dim override (added
so the stub can mimic CLaMP 3's 768-dim wire shape in dev) and the
clamp3 branch's required-env contract, without pulling torch.
"""

from __future__ import annotations

import sys

import pytest

from embedder.app import _default_embedder
from embedder.stub import DEFAULT_DIM, StubEmbedder


@pytest.fixture(autouse=True)
def _clean_env(monkeypatch: pytest.MonkeyPatch) -> None:
    # Each test starts from a known state — no backend or dim leakage
    # from the ambient shell / other tests.
    for var in ("EMBEDDER_BACKEND", "EMBEDDER_STUB_DIM", "CLAMP3_CHECKPOINT", "MERT_FOLDER"):
        monkeypatch.delenv(var, raising=False)


def test_defaults_to_stub_at_512(monkeypatch: pytest.MonkeyPatch) -> None:
    emb = _default_embedder()
    assert isinstance(emb, StubEmbedder)
    assert emb.dim == DEFAULT_DIM == 512


def test_stub_dim_override_to_768(monkeypatch: pytest.MonkeyPatch) -> None:
    # The CLaMP 3 migration changes the wire dim from CLAP's 512 to 768.
    # EMBEDDER_STUB_DIM lets dev/integration runs exercise the 768 shape
    # against the gateway without loading the real model.
    monkeypatch.setenv("EMBEDDER_STUB_DIM", "768")
    emb = _default_embedder()
    assert isinstance(emb, StubEmbedder)
    assert emb.dim == 768


def test_clamp3_requires_checkpoint_env(monkeypatch: pytest.MonkeyPatch) -> None:
    # Selecting the clamp3 backend without CLAMP3_CHECKPOINT is a config
    # error — surface the missing var as a KeyError at construction
    # rather than booting a half-configured backend.
    monkeypatch.setenv("EMBEDDER_BACKEND", "clamp3")
    monkeypatch.setenv("MERT_FOLDER", "/fake/mert")
    with pytest.raises(KeyError, match="CLAMP3_CHECKPOINT"):
        _default_embedder()


def test_clamp3_requires_mert_folder_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EMBEDDER_BACKEND", "clamp3")
    monkeypatch.setenv("CLAMP3_CHECKPOINT", "/fake/ckpt.pth")
    with pytest.raises(KeyError, match="MERT_FOLDER"):
        _default_embedder()


def test_clamp3_with_env_reaches_backend_construction(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # With both env vars set, _default_embedder routes to Clamp3Embedder.
    # torch is absent (forced), so construction fails with the [clamp3]
    # extra message — proving the wiring reaches the real backend rather
    # than silently falling through to another branch.
    monkeypatch.setenv("EMBEDDER_BACKEND", "clamp3")
    monkeypatch.setenv("CLAMP3_CHECKPOINT", "/fake/ckpt.pth")
    monkeypatch.setenv("MERT_FOLDER", "/fake/mert")
    monkeypatch.setitem(sys.modules, "torch", None)
    with pytest.raises(RuntimeError, match=r"\[clamp3\] extra"):
        _default_embedder()


def test_unknown_backend_raises(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EMBEDDER_BACKEND", "nope")
    with pytest.raises(RuntimeError, match="unknown EMBEDDER_BACKEND"):
        _default_embedder()
