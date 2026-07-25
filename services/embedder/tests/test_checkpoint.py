"""Unit tests for the shared checkpoint integrity helper.

Pure stdlib (hashlib + os) so this always runs in the lean dev/CI install
— the per-backend constructors can't reach this code without the heavy
`[clap]`/`[clamp3]` extras, so verifying the guard here is what exercises
the hardening for both backends.
"""

from __future__ import annotations

import hashlib
from typing import Any

import pytest

from embedder.checkpoint import sha256_file, verify_checkpoint

_ENV = "TEST_CHECKPOINT_SHA256"


def _ckpt(tmp_path: Any, name: str = "model.pth", data: bytes = b"fake-weights") -> str:
    p = tmp_path / name
    p.write_bytes(data)
    return str(p)


def test_sha256_file_matches_hashlib(tmp_path: Any) -> None:
    data = b"some checkpoint bytes"
    path = _ckpt(tmp_path, data=data)
    assert sha256_file(path) == hashlib.sha256(data).hexdigest()


def test_returns_digest_when_no_pin_set(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    monkeypatch.delenv(_ENV, raising=False)
    data = b"unpinned"
    path = _ckpt(tmp_path, data=data)
    assert verify_checkpoint(path, sha256_env=_ENV) == hashlib.sha256(data).hexdigest()


def test_missing_file_is_rejected(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    monkeypatch.delenv(_ENV, raising=False)
    with pytest.raises(RuntimeError, match="missing or not a regular file"):
        verify_checkpoint(str(tmp_path / "nope.pth"), sha256_env=_ENV)


def test_directory_is_rejected(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    # A directory is not a regular file — must be rejected before torch.load.
    monkeypatch.delenv(_ENV, raising=False)
    with pytest.raises(RuntimeError, match="missing or not a regular file"):
        verify_checkpoint(str(tmp_path), sha256_env=_ENV)


def test_matching_pin_loads(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    data = b"the-one-true-checkpoint"
    path = _ckpt(tmp_path, data=data)
    monkeypatch.setenv(_ENV, hashlib.sha256(data).hexdigest())
    assert verify_checkpoint(path, sha256_env=_ENV) == hashlib.sha256(data).hexdigest()


def test_matching_pin_is_case_insensitive(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    data = b"caps"
    path = _ckpt(tmp_path, data=data)
    monkeypatch.setenv(_ENV, hashlib.sha256(data).hexdigest().upper())
    # The env value is lower-cased before comparison, so an upper-case pin
    # still matches — operators paste digests in either case.
    assert verify_checkpoint(path, sha256_env=_ENV)


def test_mismatched_pin_refuses_to_load(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    monkeypatch.setenv(_ENV, "0" * 64)
    with pytest.raises(RuntimeError, match="sha256 mismatch"):
        verify_checkpoint(_ckpt(tmp_path, data=b"tampered"), sha256_env=_ENV)


def test_label_appears_in_error(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    monkeypatch.delenv(_ENV, raising=False)
    with pytest.raises(RuntimeError, match="CLaMP 3 checkpoint"):
        verify_checkpoint(
            str(tmp_path / "nope.pth"), sha256_env=_ENV, label="CLaMP 3 checkpoint"
        )
