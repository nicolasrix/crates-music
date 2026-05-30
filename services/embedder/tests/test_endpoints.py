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

from embedder.app import build_app, get_embedder
from embedder.stub import DEFAULT_DIM as STUB_DIM, StubEmbedder


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
    assert body["dim"] == STUB_DIM


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
    assert body["dim"] == STUB_DIM
    assert body["model_version"] == "stub-v1"
    assert isinstance(body["vector"], list)
    assert len(body["vector"]) == STUB_DIM
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
    assert body["dim"] == STUB_DIM
    assert len(body["vector"]) == STUB_DIM


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


# --- /reduce ---------------------------------------------------------------
#
# The reduce endpoint is the same-host counterpart to the `reduce.py`
# CLI: the gateway calls it after enough new embeddings have landed,
# the embedder opens the SQLite path directly, runs UMAP+PCA, and
# writes projection rows. Body of the endpoint = arguments of
# `reduce.run`. Response = (proj_version, written).


def test_reduce_invokes_run_with_request_params(
    app_with_loaded_stub, tmp_path, monkeypatch
):
    """Happy path that doesn't require umap-learn — monkeypatch
    `embedder.reduce.run` so we can assert on the arguments without
    pulling the full reducer stack into the test."""
    captured: dict = {}

    def fake_run(
        *,
        db_path,
        model_version,
        n_neighbors,
        min_dist,
        random_state,
        proj_version,
        n_components,
    ):
        captured["db_path"] = db_path
        captured["model_version"] = model_version
        captured["n_neighbors"] = n_neighbors
        captured["min_dist"] = min_dist
        captured["random_state"] = random_state
        captured["proj_version"] = proj_version
        captured["n_components"] = n_components
        return ("umap-v1-rs42-n15-m0p10-auto-12345", 42)

    monkeypatch.setattr("embedder.reduce.run", fake_run)

    db_file = tmp_path / "rec.sqlite"
    db_file.touch()  # path-exists check inside the handler

    client = TestClient(app_with_loaded_stub)
    r = client.post(
        "/reduce",
        json={
            "db_path": str(db_file),
            "model_version": "stub-v1",
            "n_neighbors": 20,
            "min_dist": 0.25,
            "random_state": 7,
            "proj_version": "umap-v1-rs42-n15-m0p10-auto-12345",
            "n_components": 2,
        },
    )
    assert r.status_code == 200, r.text
    body = r.json()
    assert body == {
        "proj_version": "umap-v1-rs42-n15-m0p10-auto-12345",
        "written": 42,
    }
    assert captured["db_path"] == db_file
    assert captured["model_version"] == "stub-v1"
    assert captured["n_neighbors"] == 20
    assert captured["min_dist"] == 0.25
    assert captured["random_state"] == 7
    assert captured["proj_version"] == "umap-v1-rs42-n15-m0p10-auto-12345"
    assert captured["n_components"] == 2


def test_reduce_defaults_match_reduce_module(
    app_with_loaded_stub, tmp_path, monkeypatch
):
    """When the body omits the param knobs, the handler must forward
    the same defaults the CLI uses. The contract is "request body =
    `reduce.run` keyword arguments," and silent divergence between
    the HTTP path and the CLI path is exactly the kind of bug the
    auto-trigger path will surface only weeks after the fact."""
    from embedder.reduce import (
        DEFAULT_MIN_DIST,
        DEFAULT_N_NEIGHBORS,
        DEFAULT_RANDOM_STATE,
    )

    captured: dict = {}

    def fake_run(**kwargs):
        captured.update(kwargs)
        return ("pv", 0)

    monkeypatch.setattr("embedder.reduce.run", fake_run)
    db_file = tmp_path / "rec.sqlite"
    db_file.touch()

    client = TestClient(app_with_loaded_stub)
    r = client.post(
        "/reduce",
        json={"db_path": str(db_file), "model_version": "stub-v1"},
    )
    assert r.status_code == 200, r.text
    assert captured["n_neighbors"] == DEFAULT_N_NEIGHBORS
    assert captured["min_dist"] == DEFAULT_MIN_DIST
    assert captured["random_state"] == DEFAULT_RANDOM_STATE
    assert captured["n_components"] == 2
    assert captured["proj_version"] is None


def test_reduce_returns_400_when_db_path_missing(app_with_loaded_stub, tmp_path):
    # Path validation is local to the handler — surfacing a 400 here
    # keeps the gateway's auto-trigger task from logging an opaque
    # `OperationalError` from sqlite3.
    client = TestClient(app_with_loaded_stub)
    r = client.post(
        "/reduce",
        json={
            "db_path": str(tmp_path / "missing.sqlite"),
            "model_version": "stub-v1",
        },
    )
    assert r.status_code == 400
    assert "db_path" in r.json()["detail"].lower()


def test_reduce_returns_503_when_umap_extra_missing(
    app_with_loaded_stub, tmp_path, monkeypatch
):
    # The `reduce` extra (umap-learn + sklearn) is optional. Production
    # deployments install it; dev installs often don't. Surface the
    # missing dependency as 503 with a recognisable detail so the
    # gateway can log "service degraded" instead of treating it as a
    # generic 5xx.
    def fake_run(**kwargs):
        raise ImportError("No module named 'umap'")

    monkeypatch.setattr("embedder.reduce.run", fake_run)
    db_file = tmp_path / "rec.sqlite"
    db_file.touch()

    client = TestClient(app_with_loaded_stub)
    r = client.post(
        "/reduce",
        json={"db_path": str(db_file), "model_version": "stub-v1"},
    )
    assert r.status_code == 503
    assert "umap" in r.json()["detail"].lower() or "reduce" in r.json()["detail"].lower()


def test_reduce_does_not_require_model_loaded(
    app_with_unloaded_stub, tmp_path, monkeypatch
):
    # The reducer reads stored embeddings — it doesn't run inference,
    # so `model_loaded=False` is not a reason to refuse the call.
    # The sidecar can be mid-CLAP-warmup and still service /reduce.
    def fake_run(**kwargs):
        return ("pv", 0)

    monkeypatch.setattr("embedder.reduce.run", fake_run)
    db_file = tmp_path / "rec.sqlite"
    db_file.touch()

    client = TestClient(app_with_unloaded_stub)
    r = client.post(
        "/reduce",
        json={"db_path": str(db_file), "model_version": "stub-v1"},
    )
    assert r.status_code == 200, r.text


# --- bearer auth (optional, for split-host deployments) -------------------
#
# When EMBEDDER_BEARER_TOKEN is set in the environment the embedder
# requires `Authorization: Bearer <token>` on /embed/* and /reduce.
# /healthz stays open — boot probes shouldn't need to be told the
# secret, and a 200 from /healthz doesn't leak compute time.


@pytest.fixture
def secured_app(monkeypatch):
    monkeypatch.setenv("EMBEDDER_BEARER_TOKEN", "shared-secret")
    app = build_app()
    stub = StubEmbedder(model_version="stub-v1", loaded=True)
    app.dependency_overrides[get_embedder] = lambda: stub
    return app


def test_auth_healthz_stays_open_when_token_set(secured_app):
    client = TestClient(secured_app)
    r = client.get("/healthz")
    assert r.status_code == 200


def test_auth_embed_audio_401_without_bearer(secured_app):
    client = TestClient(secured_app)
    r = client.post(
        "/embed/audio",
        content=b"\x00" * 64,
        headers={"content-type": "application/octet-stream"},
    )
    assert r.status_code == 401, r.text


def test_auth_embed_audio_401_with_wrong_bearer(secured_app):
    client = TestClient(secured_app)
    r = client.post(
        "/embed/audio",
        content=b"\x00" * 64,
        headers={
            "content-type": "application/octet-stream",
            "Authorization": "Bearer not-the-token",
        },
    )
    assert r.status_code == 401, r.text


def test_auth_embed_audio_200_with_correct_bearer(secured_app):
    client = TestClient(secured_app)
    r = client.post(
        "/embed/audio",
        content=b"\x00" * 64,
        headers={
            "content-type": "application/octet-stream",
            "Authorization": "Bearer shared-secret",
        },
    )
    assert r.status_code == 200, r.text


def test_auth_embed_text_401_without_bearer(secured_app):
    client = TestClient(secured_app)
    r = client.post("/embed/text", json={"text": "hello"})
    assert r.status_code == 401, r.text


def test_auth_reduce_401_without_bearer(secured_app, monkeypatch, tmp_path):
    # /reduce also opens the gateway's DB file — privileged.
    monkeypatch.setattr("embedder.reduce.run", lambda **_: ("pv", 0))
    db_file = tmp_path / "rec.sqlite"
    db_file.touch()
    client = TestClient(secured_app)
    r = client.post(
        "/reduce",
        json={"db_path": str(db_file), "model_version": "stub-v1"},
    )
    assert r.status_code == 401, r.text


def test_auth_no_token_set_allows_everything(monkeypatch):
    # Default behaviour: env var absent → no enforcement, calls pass
    # through. Single-host deployments don't need to set anything.
    monkeypatch.delenv("EMBEDDER_BEARER_TOKEN", raising=False)
    app = build_app()
    stub = StubEmbedder(model_version="stub-v1", loaded=True)
    app.dependency_overrides[get_embedder] = lambda: stub
    client = TestClient(app)
    r = client.post(
        "/embed/audio",
        content=b"\x00" * 64,
        headers={"content-type": "application/octet-stream"},
    )
    assert r.status_code == 200, r.text


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
