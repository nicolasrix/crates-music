"""Endpoint tests for the embedder sidecar.

These run with a `StubEmbedder` injected via FastAPI's dependency
overrides so the suite doesn't need the CLAP weights or PyTorch. The
stub returns deterministic vectors derived from a hash of the input,
which is enough to verify the wire shape, error paths, and that the
server respects the model_loaded flag.
"""

from __future__ import annotations

import logging

import pytest
from fastapi.testclient import TestClient

from embedder.app import _auth_disabled_warning, build_app, get_embedder
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


def test_embed_audio_rejects_oversized_body(app_with_loaded_stub, monkeypatch):
    # Guardrail against a runaway payload exhausting embedder memory.
    # Shrink the cap so the test stays cheap rather than allocating 64 MiB.
    monkeypatch.setattr("embedder.app.MAX_AUDIO_BYTES", 8)
    client = TestClient(app_with_loaded_stub)
    r = client.post(
        "/embed/audio",
        content=b"way more than eight bytes",
        headers={"content-type": "application/octet-stream"},
    )
    assert r.status_code == 413
    assert "too large" in r.json()["detail"].lower()


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
# Vectors over the wire: the gateway reads its own recommend DB, packs
# the embedding matrix as base64 little-endian f32, and POSTs it here.
# The embedder is pure compute — it decodes the matrix, runs UMAP+PCA
# via `reduce.project_matrix`, and returns one point per track. It does
# NOT open any SQLite file (that assumed a shared volume, which breaks
# when the embedder runs on a separate GPU host). The gateway persists
# the returned coordinates itself.

import base64
import struct

from embedder.reduce import Projection2D


def _pack(vectors: list[list[float]]) -> str:
    """Pack a matrix the way the gateway's `EmbedderClient::reduce`
    does — row-major little-endian f32, base64."""
    raw = b"".join(struct.pack(f"<{len(row)}f", *row) for row in vectors)
    return base64.b64encode(raw).decode("ascii")


def test_reduce_decodes_matrix_and_returns_points(
    app_with_loaded_stub, monkeypatch
):
    """Happy path without umap-learn — monkeypatch
    `embedder.reduce.project_matrix` so we can assert on the decoded
    matrix and forwarded params without pulling in the reducer stack."""
    captured: dict = {}

    def fake_project_matrix(
        track_ids, matrix, *, n_neighbors, min_dist, random_state, metric, n_components
    ):
        captured["track_ids"] = list(track_ids)
        captured["matrix"] = matrix.tolist()
        captured["n_neighbors"] = n_neighbors
        captured["min_dist"] = min_dist
        captured["random_state"] = random_state
        captured["metric"] = metric
        captured["n_components"] = n_components
        return [
            Projection2D(track_id="t1", x=0.5, y=-0.5, pc1=0.1, pc2=0.2),
            Projection2D(track_id="t2", x=1.5, y=-1.5, z=None),
        ]

    monkeypatch.setattr("embedder.reduce.project_matrix", fake_project_matrix)

    client = TestClient(app_with_loaded_stub)
    r = client.post(
        "/reduce",
        json={
            "track_ids": ["t1", "t2"],
            "dim": 2,
            "vectors_b64": _pack([[1.0, 2.0], [3.0, 4.0]]),
            "n_neighbors": 20,
            "min_dist": 0.25,
            "random_state": 7,
            "n_components": 2,
            "metric": "euclidean",
        },
    )
    assert r.status_code == 200, r.text
    body = r.json()
    assert [p["track_id"] for p in body["points"]] == ["t1", "t2"]
    assert body["points"][0] == {
        "track_id": "t1",
        "x": 0.5,
        "y": -0.5,
        "z": None,
        "pc1": 0.1,
        "pc2": 0.2,
        "pc3": None,
        "pc4": None,
    }
    # Matrix decoded row-major at the announced dim.
    assert captured["track_ids"] == ["t1", "t2"]
    assert captured["matrix"] == [[1.0, 2.0], [3.0, 4.0]]
    assert captured["n_neighbors"] == 20
    assert captured["min_dist"] == 0.25
    assert captured["random_state"] == 7
    assert captured["metric"] == "euclidean"
    assert captured["n_components"] == 2


def test_reduce_defaults_match_reduce_module(app_with_loaded_stub, monkeypatch):
    """When the body omits the knobs, the handler forwards the same
    defaults the CLI uses. Silent divergence between the HTTP path and
    the reducer module is exactly the bug the auto-trigger surfaces
    weeks later."""
    from embedder.reduce import (
        DEFAULT_METRIC,
        DEFAULT_MIN_DIST,
        DEFAULT_N_NEIGHBORS,
        DEFAULT_RANDOM_STATE,
    )

    captured: dict = {}

    def fake_project_matrix(track_ids, matrix, **kwargs):
        captured.update(kwargs)
        return []

    monkeypatch.setattr("embedder.reduce.project_matrix", fake_project_matrix)

    client = TestClient(app_with_loaded_stub)
    r = client.post(
        "/reduce",
        json={
            "track_ids": ["t1"],
            "dim": 2,
            "vectors_b64": _pack([[1.0, 2.0]]),
        },
    )
    assert r.status_code == 200, r.text
    assert captured["n_neighbors"] == DEFAULT_N_NEIGHBORS
    assert captured["min_dist"] == DEFAULT_MIN_DIST
    assert captured["random_state"] == DEFAULT_RANDOM_STATE
    assert captured["metric"] == DEFAULT_METRIC
    assert captured["n_components"] == 2


def test_reduce_returns_400_on_byte_length_mismatch(app_with_loaded_stub):
    # The decoded matrix must be exactly N*dim*4 bytes. A mismatch is a
    # caller bug; surface a 400 with a clear detail rather than letting
    # numpy raise an opaque reshape error.
    client = TestClient(app_with_loaded_stub)
    r = client.post(
        "/reduce",
        json={
            "track_ids": ["t1", "t2"],
            "dim": 2,
            # Only one row's worth of bytes for two track_ids.
            "vectors_b64": _pack([[1.0, 2.0]]),
        },
    )
    assert r.status_code == 400
    assert "vectors_b64" in r.json()["detail"]


def test_reduce_returns_503_when_umap_extra_missing(
    app_with_loaded_stub, monkeypatch
):
    # The `reduce` extra (umap-learn + sklearn) is optional. Surface the
    # missing dependency as 503 so the gateway logs "service degraded"
    # rather than treating it as a generic 5xx.
    def fake_project_matrix(track_ids, matrix, **kwargs):
        raise ImportError("No module named 'umap'")

    monkeypatch.setattr("embedder.reduce.project_matrix", fake_project_matrix)

    client = TestClient(app_with_loaded_stub)
    r = client.post(
        "/reduce",
        json={"track_ids": ["t1"], "dim": 2, "vectors_b64": _pack([[1.0, 2.0]])},
    )
    assert r.status_code == 503
    assert "umap" in r.json()["detail"].lower() or "reduce" in r.json()["detail"].lower()


def test_reduce_returns_400_on_bad_params(app_with_loaded_stub, monkeypatch):
    # Knobs only the reducer can validate (an unknown metric, n_neighbors ≥ N)
    # raise ValueError from the reducer — a caller bug, surfaced as 400
    # rather than an opaque 500. (Out-of-range n_components is rejected
    # earlier by Pydantic as 422 — see test_reduce_rejects_bad_n_components.)
    def fake_project_matrix(track_ids, matrix, **kwargs):
        raise ValueError("unknown metric 'bogus'")

    monkeypatch.setattr("embedder.reduce.project_matrix", fake_project_matrix)

    client = TestClient(app_with_loaded_stub)
    r = client.post(
        "/reduce",
        json={
            "track_ids": ["t1"],
            "dim": 2,
            "vectors_b64": _pack([[1.0, 2.0]]),
            "metric": "bogus",
        },
    )
    assert r.status_code == 400
    assert "metric" in r.json()["detail"]


def test_reduce_rejects_bad_n_components(app_with_loaded_stub):
    # n_components is bounded to {2, 3} at the schema level, so an
    # out-of-range value is a 422 validation error before the handler runs.
    client = TestClient(app_with_loaded_stub)
    r = client.post(
        "/reduce",
        json={
            "track_ids": ["t1"],
            "dim": 2,
            "vectors_b64": _pack([[1.0, 2.0]]),
            "n_components": 5,
        },
    )
    assert r.status_code == 422


def test_reduce_rejects_malformed_base64(app_with_loaded_stub):
    # Non-base64 garbage in vectors_b64 is a caller bug → 400, not a 500.
    client = TestClient(app_with_loaded_stub)
    r = client.post(
        "/reduce",
        json={
            "track_ids": ["t1"],
            "dim": 2,
            "vectors_b64": "!!!not base64!!!",
        },
    )
    assert r.status_code == 400
    assert "base64" in r.json()["detail"]


def test_reduce_does_not_require_model_loaded(
    app_with_unloaded_stub, monkeypatch
):
    # /reduce is pure compute, not inference — `model_loaded=False` is
    # not a reason to refuse. The sidecar can be mid-CLAP-warmup and
    # still service a reduction.
    def fake_project_matrix(track_ids, matrix, **kwargs):
        return []

    monkeypatch.setattr("embedder.reduce.project_matrix", fake_project_matrix)

    client = TestClient(app_with_unloaded_stub)
    r = client.post(
        "/reduce",
        json={"track_ids": ["t1"], "dim": 2, "vectors_b64": _pack([[1.0, 2.0]])},
    )
    assert r.status_code == 200, r.text


# --- bearer auth (optional, for split-host deployments) -------------------
#
# When EMBEDDER_BEARER_TOKEN is set in the environment the embedder
# requires `Authorization: Bearer <token>` on /embed/* and /reduce.
# /healthz stays reachable for liveness probes — boot probes shouldn't
# need the secret — but it redacts the descriptive fields
# (model_version, dim, device) for unauthenticated callers so a LAN peer
# can't fingerprint the model/hardware.


@pytest.fixture
def secured_app(monkeypatch):
    monkeypatch.setenv("EMBEDDER_BEARER_TOKEN", "shared-secret")
    app = build_app()
    stub = StubEmbedder(model_version="stub-v1", loaded=True)
    app.dependency_overrides[get_embedder] = lambda: stub
    return app


def test_auth_healthz_stays_open_when_token_set(secured_app):
    # Liveness must not require the secret — Docker HEALTHCHECK / readiness
    # probes hit /healthz without a bearer.
    client = TestClient(secured_app)
    r = client.get("/healthz")
    assert r.status_code == 200
    body = r.json()
    assert body["status"] == "ok"
    assert body["model_loaded"] is True


def test_auth_healthz_redacts_descriptive_fields_without_bearer(secured_app):
    # An unauthenticated peer must not be able to fingerprint the model
    # or hardware: model_version / dim / device are withheld.
    client = TestClient(secured_app)
    body = client.get("/healthz").json()
    assert "model_version" not in body
    assert "dim" not in body
    assert "device" not in body


def test_auth_healthz_full_body_with_correct_bearer(secured_app):
    # The gateway boot probe carries the bearer, so split-host deploys
    # still get the descriptive fields they read at startup.
    client = TestClient(secured_app)
    body = client.get(
        "/healthz", headers={"Authorization": "Bearer shared-secret"}
    ).json()
    assert body["model_version"] == "stub-v1"
    assert body["dim"] == STUB_DIM
    assert body["device"] == "cpu"


def test_auth_healthz_redacts_with_wrong_bearer(secured_app):
    # A wrong token is treated like no token for /healthz — liveness
    # only, no fingerprinting. (It's a hard 401 on the compute endpoints.)
    client = TestClient(secured_app)
    r = client.get("/healthz", headers={"Authorization": "Bearer nope"})
    assert r.status_code == 200
    assert "model_version" not in r.json()


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


def test_auth_no_token_stub_backend_allows_everything(monkeypatch, caplog):
    # Default dev behaviour: stub backend + no token → no enforcement,
    # calls pass through, and NO warning is emitted (loopback dev is the
    # intended fail-open case).
    monkeypatch.delenv("EMBEDDER_BEARER_TOKEN", raising=False)
    monkeypatch.delenv("EMBEDDER_BACKEND", raising=False)
    with caplog.at_level(logging.WARNING, logger="embedder"):
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
    assert not any("UNAUTHENTICATED" in rec.message for rec in caplog.records)


def test_auth_no_token_real_backend_warns(monkeypatch, caplog):
    # Canary: a real model backend with no token is the split-host
    # footgun — auth is silently off. We don't hard-fail (loopback
    # deployments stay working), but the warning must be impossible to
    # miss. Inject a stub so no model/torch load happens; the warning
    # keys off EMBEDDER_BACKEND, not the injected embedder.
    monkeypatch.delenv("EMBEDDER_BEARER_TOKEN", raising=False)
    monkeypatch.setenv("EMBEDDER_BACKEND", "clamp3")
    stub = StubEmbedder(model_version="stub-v1", loaded=True)
    with caplog.at_level(logging.WARNING, logger="embedder"):
        app = build_app(embedder=stub)
    assert app.state.bearer_token is None
    assert any("UNAUTHENTICATED" in rec.message for rec in caplog.records)


def test_auth_disabled_warning_decision():
    # Pure decision helper: warn only for a non-stub backend with no token.
    assert _auth_disabled_warning("stub", None) is None
    assert _auth_disabled_warning("clamp3", "secret") is None
    assert _auth_disabled_warning("clap", None) is not None
    assert _auth_disabled_warning("clamp3", None) is not None


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
