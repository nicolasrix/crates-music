"""Tests for stage-level timing exposed via the `Server-Timing` HTTP header.

The gateway uses these timings to attribute time inside `/embed/audio`
to its sub-stages (decode, resample, GPU forward). Without this signal,
"the embedder took 600 ms" is opaque — with it, we can pinpoint whether
the bottleneck is on the CPU side (decode) or the device (forward).

Two surfaces under test here:
1. The pure formatter `format_server_timing(stages)` — no I/O.
2. The HTTP handler — emits the header for successful inference.
"""

from __future__ import annotations

import pytest
from fastapi.testclient import TestClient

from embedder.app import build_app, get_embedder
from embedder.protocol import EmbedResult
from embedder.stub import StubEmbedder


# --- pure formatter ---------------------------------------------------------


def test_format_server_timing_emits_one_entry_per_stage():
    from embedder.app import format_server_timing

    out = format_server_timing({"decode": 42.0, "gpu_forward": 520.0})
    # Order matches insertion (Python dict preserves it). One entry
    # per stage, semicolons separate metric and dur, comma-space
    # between entries — per RFC for Server-Timing.
    assert out == "decode;dur=42, gpu_forward;dur=520"


def test_format_server_timing_keeps_one_decimal_for_subms_values():
    from embedder.app import format_server_timing

    # Sub-millisecond resolution is useful when stages are fast (the
    # stub's hashing takes ~0.1 ms). We round to one decimal so the
    # header stays compact while still preserving "stage was nonzero".
    out = format_server_timing({"hash": 0.12, "wrap": 1.78})
    assert out == "hash;dur=0.1, wrap;dur=1.8"


def test_format_server_timing_returns_empty_for_empty_mapping():
    from embedder.app import format_server_timing

    assert format_server_timing({}) == ""


def test_format_server_timing_skips_entries_with_invalid_names():
    from embedder.app import format_server_timing

    # Server-Timing names must be tokens (no spaces / commas / etc.).
    # Rather than sanitize silently, we drop bad names entirely so a
    # backend bug surfaces as "missing stage" rather than a malformed
    # header that breaks parsers downstream.
    out = format_server_timing({"good_one": 5.0, "bad name with spaces": 10.0})
    assert "good_one" in out
    assert "bad name" not in out


# --- HTTP handler --------------------------------------------------------


@pytest.fixture
def client_with_loaded_stub() -> TestClient:
    app = build_app()
    stub = StubEmbedder(model_version="stub-v1", loaded=True)
    app.dependency_overrides[get_embedder] = lambda: stub
    return TestClient(app)


def test_embed_audio_emits_server_timing_header(
    client_with_loaded_stub: TestClient,
) -> None:
    r = client_with_loaded_stub.post(
        "/embed/audio",
        content=b"x" * 256,
        headers={"content-type": "application/octet-stream"},
    )
    assert r.status_code == 200
    assert "server-timing" in {k.lower() for k in r.headers.keys()}
    header = r.headers["server-timing"]
    assert header, "Server-Timing should be non-empty"
    # Expect the stub's single stage ("hash") to be present.
    assert "hash" in header


def test_embed_audio_server_timing_durations_are_positive(
    client_with_loaded_stub: TestClient,
) -> None:
    r = client_with_loaded_stub.post(
        "/embed/audio",
        content=b"y" * 256,
        headers={"content-type": "application/octet-stream"},
    )
    header = r.headers["server-timing"]
    # Header looks like "stage;dur=X.Y, stage2;dur=Z" — pull every dur=.
    durs = [
        float(part.split("dur=", 1)[1])
        for part in header.split(",")
        if "dur=" in part
    ]
    assert durs, "header had no dur= entries"
    assert all(d >= 0 for d in durs), f"negative duration in {durs}"


def test_embed_text_emits_server_timing_header(
    client_with_loaded_stub: TestClient,
) -> None:
    r = client_with_loaded_stub.post(
        "/embed/text", json={"text": "rainy sunday"}
    )
    assert r.status_code == 200
    assert "server-timing" in {k.lower() for k in r.headers.keys()}


def test_embed_audio_503_does_not_emit_server_timing() -> None:
    # When the model isn't loaded we short-circuit before any inference,
    # so there's nothing to time. No header avoids confusing the parser.
    app = build_app()
    stub = StubEmbedder(model_version="stub-v1", loaded=False)
    app.dependency_overrides[get_embedder] = lambda: stub
    client = TestClient(app)
    r = client.post(
        "/embed/audio",
        content=b"\x00" * 32,
        headers={"content-type": "application/octet-stream"},
    )
    assert r.status_code == 503
    assert "server-timing" not in {k.lower() for k in r.headers.keys()}


# --- EmbedResult shape ---------------------------------------------------


def test_stub_embed_audio_returns_embed_result_with_stages() -> None:
    stub = StubEmbedder(model_version="stub-v1", loaded=True)
    out = stub.embed_audio(b"some bytes")
    assert isinstance(out, EmbedResult)
    assert out.vector.shape == (512,)
    assert out.stages_ms, "stub should report at least one timing stage"
    assert all(v >= 0 for v in out.stages_ms.values())


def test_stub_embed_text_returns_embed_result_with_stages() -> None:
    stub = StubEmbedder(model_version="stub-v1", loaded=True)
    out = stub.embed_text("query")
    assert isinstance(out, EmbedResult)
    assert out.vector.shape == (512,)
    assert out.stages_ms
