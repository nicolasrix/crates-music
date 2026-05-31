"""Shared model-checkpoint integrity helpers.

``torch.load`` unpickles, so a tampered or swapped ``.pt``/``.pth`` could in
principle run arbitrary code at load time. The backends already pass
``weights_only=True`` to neutralise the pickle-RCE vector; the helpers here
add a second, cheaper layer on top:

- reject anything that isn't a regular file (a dir / missing path / device
  node) *before* the path ever reaches ``torch.load``;
- log the SHA-256 so an operator can discover the digest to pin;
- when a per-backend ``*_CHECKPOINT_SHA256`` env var is set, refuse to load
  anything whose digest doesn't match — a deterministic guard against an
  accidentally-swapped or tampered checkpoint silently changing embeddings.

Kept dependency-free (stdlib only) so it imports without the heavy `[clap]`
/ `[clamp3]` extras and can be unit-tested in the lean dev/CI install.
"""

from __future__ import annotations

import hashlib
import logging
import os

logger = logging.getLogger(__name__)


def sha256_file(path: str, chunk_size: int = 1 << 20) -> str:
    """Stream a file through SHA-256 without loading it all into memory."""
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(chunk_size), b""):
            h.update(chunk)
    return h.hexdigest()


def verify_checkpoint(path: str, *, sha256_env: str, label: str = "checkpoint") -> str:
    """Validate a checkpoint before it is handed to ``torch.load``.

    Args:
        path: filesystem path to the checkpoint.
        sha256_env: name of the env var holding the expected hex digest;
            when set (and non-empty) the digest must match or this raises.
        label: human-readable name used in log lines and error messages
            (e.g. ``"CLaMP 3 checkpoint"``).

    Returns:
        The computed SHA-256 hex digest.

    Raises:
        RuntimeError: if ``path`` is not a regular file, or the pin is set
            and the digest doesn't match.
    """
    if not os.path.isfile(path):
        raise RuntimeError(f"{label} missing or not a regular file: {path}")
    digest = sha256_file(path)
    logger.info("%s sha256=%s (%s)", label, digest, path)
    expected = os.environ.get(sha256_env, "").strip().lower()
    if expected and digest != expected:
        raise RuntimeError(
            f"{label} sha256 mismatch — refusing to load. "
            f"expected={expected} actual={digest}. "
            f"Unset {sha256_env} only if you intend to change the model."
        )
    return digest
