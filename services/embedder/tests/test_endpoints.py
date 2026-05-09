"""Endpoint tests for the embedder sidecar.

These run with a `StubEmbedder` injected via FastAPI's dependency
overrides so the suite doesn't need the CLAP weights or PyTorch. The
stub returns deterministic vectors derived from a hash of the input,
which is enough to verify the wire shape, error paths, and that the
server respects the model_loaded flag.
"""

from __future__ import annotations

import pytest
from fastapi.testclient import TestClient

from embedder.app import EMBEDDING_DIM, build_app, get_embedder
from embedder.stub import StubEmbedder


@pytest.fixture
def app_with_loaded_stub():
    app = build_app()
    stub = StubEmbedder(model_version="stub-v1", loaded=True)
    app.dependency_overrides[get_embedder] = lambda: stub
    return app


@pytest.fixture
def app_with_unloaded_stub():
    app = build_app()
    stub = StubEmbedder(model_version="stub-v1", loaded=False)
    app.dependency_overrides[get_embedder] = lambda: stub
    return app


def test_healthz_returns_200_when_loaded(app_with_loaded_stub):
    client = TestClient(app_with_loaded_stub)
    r = client.get("/healthz")
    assert r.status_code == 200
    body = r.json()
    assert body["model_loaded"] is True
    assert body["model_version"] == "stub-v1"
    assert body["dim"] == EMBEDDING_DIM


def test_healthz_reports_device_for_stub(app_with_loaded_stub):
    # Stub always runs on CPU. The field exists so callers (the gateway
    # boot probe) can detect silent CPU fallback after a ROCm install.
    client = TestClient(app_with_loaded_stub)
    body = client.get("/healthz").json()
    assert body["device"] == "cpu"


def test_healthz_reports_device_when_unloaded(app_with_unloaded_stub):
    # device is reported regardless of load state — useful when probing
    # a still-loading sidecar to see whether GPU was selected at all.
    client = TestClient(app_with_unloaded_stub)
    body = client.get("/healthz").json()
    assert body["device"] == "cpu"


def test_healthz_returns_503_when_not_loaded(app_with_unloaded_stub):
    client = TestClient(app_with_unloaded_stub)
    r = client.get("/healthz")
    assert r.status_code == 503
    body = r.json()
    assert body["model_loaded"] is False


def test_embed_audio_returns_vector(app_with_loaded_stub):
    client = TestClient(app_with_loaded_stub)
    fake_audio = b"\x00" * 1024  # raw bytes, content doesn't matter for stub
    r = client.post(
        "/embed/audio",
        content=fake_audio,
        headers={"content-type": "application/octet-stream"},
    )
    assert r.status_code == 200, r.text
    body = r.json()
    assert body["dim"] == EMBEDDING_DIM
    assert body["model_version"] == "stub-v1"
    assert isinstance(body["vector"], list)
    assert len(body["vector"]) == EMBEDDING_DIM
    assert all(isinstance(x, float) for x in body["vector"])


def test_embed_audio_is_deterministic(app_with_loaded_stub):
    client = TestClient(app_with_loaded_stub)
    payload = b"hello world" * 100
    r1 = client.post(
        "/embed/audio",
        content=payload,
        headers={"content-type": "application/octet-stream"},
    )
    r2 = client.post(
        "/embed/audio",
        content=payload,
        headers={"content-type": "application/octet-stream"},
    )
    assert r1.json()["vector"] == r2.json()["vector"]


def test_embed_audio_different_input_different_vector(app_with_loaded_stub):
    client = TestClient(app_with_loaded_stub)
    r1 = client.post(
        "/embed/audio",
        content=b"alpha",
        headers={"content-type": "application/octet-stream"},
    )
    r2 = client.post(
        "/embed/audio",
        content=b"beta",
        headers={"content-type": "application/octet-stream"},
    )
    assert r1.json()["vector"] != r2.json()["vector"]


def test_embed_audio_rejects_empty_body(app_with_loaded_stub):
    client = TestClient(app_with_loaded_stub)
    r = client.post(
        "/embed/audio",
        content=b"",
        headers={"content-type": "application/octet-stream"},
    )
    assert r.status_code == 400
    assert "empty" in r.json()["detail"].lower()


def test_embed_audio_returns_503_when_not_loaded(app_with_unloaded_stub):
    client = TestClient(app_with_unloaded_stub)
    r = client.post(
        "/embed/audio",
        content=b"\x00" * 100,
        headers={"content-type": "application/octet-stream"},
    )
    assert r.status_code == 503


def test_embed_text_returns_vector(app_with_loaded_stub):
    client = TestClient(app_with_loaded_stub)
    r = client.post("/embed/text", json={"text": "rainy sunday afternoon"})
    assert r.status_code == 200, r.text
    body = r.json()
    assert body["dim"] == EMBEDDING_DIM
    assert len(body["vector"]) == EMBEDDING_DIM


def test_embed_text_rejects_missing_field(app_with_loaded_stub):
    client = TestClient(app_with_loaded_stub)
    r = client.post("/embed/text", json={})
    assert r.status_code == 422  # pydantic validation


def test_embed_text_accepts_empty_string(app_with_loaded_stub):
    # CLAP's text encoder doesn't error on empty strings — it just
    # produces a degenerate embedding. We pass through; rejecting at
    # the API layer would be wrong since the user might genuinely
    # want to query the "no-text" centroid.
    client = TestClient(app_with_loaded_stub)
    r = client.post("/embed/text", json={"text": ""})
    assert r.status_code == 200


def test_embed_text_returns_503_when_not_loaded(app_with_unloaded_stub):
    client = TestClient(app_with_unloaded_stub)
    r = client.post("/embed/text", json={"text": "anything"})
    assert r.status_code == 503


def test_vectors_are_l2_normalized(app_with_loaded_stub):
    # CLAP outputs are approximately L2-normalized. The stub follows
    # the same convention so callers can assume cosine ≈ dot product.
    import math

    client = TestClient(app_with_loaded_stub)
    r = client.post(
        "/embed/audio",
        content=b"some bytes here",
        headers={"content-type": "application/octet-stream"},
    )
    v = r.json()["vector"]
    norm = math.sqrt(sum(x * x for x in v))
    assert abs(norm - 1.0) < 1e-5, f"vector not L2-normalized: norm={norm}"
