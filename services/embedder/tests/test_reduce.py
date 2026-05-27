"""Tests for `embedder.reduce`.

The UMAP-dependent test is gated behind `pytest.importorskip` so the
suite still passes when the optional `reduce` extra isn't installed
(uv-managed dev workflow doesn't pull umap-learn by default). Pure
helpers (vector decode, proj_version derivation, SQLite I/O) run
unconditionally.
"""

from __future__ import annotations

import sqlite3
import struct
import time
from pathlib import Path

import numpy as np
import pytest

from embedder.reduce import (
    Embedding,
    Projection2D,
    compute_pcs,
    decode_vector_blob,
    default_proj_version,
    read_embeddings_from_sqlite,
    run,
    write_projections_to_sqlite,
)

# Mirrors the music-recommend migrations 0001 + 0006. Inlined so the
# tests don't need to invoke sqlx — keeping the Python side standalone.
SCHEMA = """
CREATE TABLE track_embeddings (
    track_id        TEXT    NOT NULL,
    model_version   TEXT    NOT NULL,
    dim             INTEGER NOT NULL CHECK (dim >= 0),
    vector          BLOB,
    status          TEXT    NOT NULL
                            CHECK (status IN ('not_started', 'in_progress', 'done', 'failed')),
    error           TEXT,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    PRIMARY KEY (track_id, model_version)
);
CREATE TABLE embedding_projection_2d (
    track_id        TEXT    NOT NULL,
    model_version   TEXT    NOT NULL,
    proj_version    TEXT    NOT NULL,
    x               REAL    NOT NULL,
    y               REAL    NOT NULL,
    created_at_ms   INTEGER NOT NULL,
    pc1             REAL,
    pc2             REAL,
    pc3             REAL,
    pc4             REAL,
    z               REAL,
    PRIMARY KEY (track_id, model_version, proj_version)
) WITHOUT ROWID;
"""


def _pack_vector(v: np.ndarray) -> bytes:
    """Inverse of `decode_vector_blob` — little-endian f32 packing,
    matching the Rust side that writes these rows."""
    return struct.pack(f"<{v.size}f", *v.astype(np.float32))


@pytest.fixture
def db(tmp_path: Path) -> sqlite3.Connection:
    conn = sqlite3.connect(tmp_path / "rec.sqlite")
    conn.executescript(SCHEMA)
    return conn


# --- pure helpers ---------------------------------------------------------


def test_decode_vector_blob_round_trips_f32_values() -> None:
    v = np.array([1.0, -2.5, 3.25], dtype=np.float32)
    decoded = decode_vector_blob(_pack_vector(v), dim=3)
    np.testing.assert_array_equal(decoded, v)


def test_decode_vector_blob_rejects_wrong_length() -> None:
    with pytest.raises(ValueError, match="does not match"):
        decode_vector_blob(b"\x00" * 5, dim=3)


def test_default_proj_version_encodes_params_stably() -> None:
    pv = default_proj_version(n_neighbors=15, min_dist=0.1, random_state=42)
    assert pv == "umap-v1-rs42-n15-m0p10"


def test_default_proj_version_changes_with_any_param() -> None:
    base = default_proj_version()
    assert base != default_proj_version(n_neighbors=30)
    assert base != default_proj_version(min_dist=0.25)
    assert base != default_proj_version(random_state=7)


def test_default_proj_version_omits_dim_suffix_for_2d() -> None:
    # Existing 2D names must stay stable so re-running the reducer with
    # defaults idempotently overwrites the same proj_version rather than
    # spawning a parallel `-d2` copy.
    assert default_proj_version(n_components=2) == default_proj_version()


def test_default_proj_version_appends_d3_for_3d_run() -> None:
    pv = default_proj_version(
        n_neighbors=15, min_dist=0.1, random_state=42, n_components=3
    )
    assert pv == "umap-v1-rs42-n15-m0p10-d3"


def test_default_proj_version_rejects_unsupported_dim() -> None:
    # n_components is the colour-channel knob, not a full reducer config.
    # Anything other than 2 (canvas-only) or 3 (canvas + colour z) means
    # the persistence schema can't represent it — surface that early.
    with pytest.raises(ValueError, match="n_components"):
        default_proj_version(n_components=4)


# --- PCA -------------------------------------------------------------------
#
# PCA gives us "honest" extra dimensions of the latent space — axes
# ordered by variance in the original 512-D CLAP vectors. UMAP picks
# coordinates optimized for cluster legibility; PCA picks coordinates
# optimized for variance preservation. Both views are kept under one
# proj_version so they describe the same snapshot.


@pytest.fixture
def sklearn_module():
    """Gate PCA tests on scikit-learn availability — it's a transitive
    dep of the `reduce` extra, not installed in the base dev profile."""
    return pytest.importorskip(
        "sklearn",
        reason="install with `uv sync --extra reduce` to run PCA-backed tests",
    )


def test_compute_pcs_returns_shape_n_by_requested_components(
    sklearn_module,  # noqa: ARG001
) -> None:
    # 30 points × 8 dims → 4 PCs, one per requested component, no padding.
    rng = np.random.default_rng(0)
    matrix = rng.standard_normal((30, 8)).astype(np.float32)
    pcs = compute_pcs(matrix, n_components=4)
    assert pcs.shape == (30, 4)
    assert np.isfinite(pcs).all()


def test_compute_pcs_clamps_components_to_n_when_few_points(
    sklearn_module,  # noqa: ARG001
) -> None:
    # sklearn's PCA can't produce more components than min(N-1, D).
    # We clamp internally so a 3-point input doesn't crash — the
    # reducer must keep working on tiny dev datasets.
    rng = np.random.default_rng(0)
    matrix = rng.standard_normal((3, 8)).astype(np.float32)
    pcs = compute_pcs(matrix, n_components=4)
    # Clamp keeps shape compatible: rows = N, cols ≤ requested. The
    # caller pads to 4 with None when persisting.
    assert pcs.shape[0] == 3
    assert pcs.shape[1] <= 4
    assert pcs.shape[1] >= 1


def test_compute_pcs_clamps_components_to_dimensionality(
    sklearn_module,  # noqa: ARG001
) -> None:
    # Embeddings can't have more axes than their own dimensionality.
    rng = np.random.default_rng(0)
    matrix = rng.standard_normal((10, 2)).astype(np.float32)
    pcs = compute_pcs(matrix, n_components=4)
    assert pcs.shape == (10, 2)


def test_compute_pcs_is_deterministic_with_seeded_input(
    sklearn_module,  # noqa: ARG001
) -> None:
    rng = np.random.default_rng(7)
    matrix = rng.standard_normal((20, 8)).astype(np.float32)
    a = compute_pcs(matrix, n_components=4)
    b = compute_pcs(matrix, n_components=4)
    # PCA is a closed-form solve; same inputs must yield identical
    # outputs to within FP rounding. Up to sign — PCs are sign-
    # arbitrary, so compare absolute values.
    np.testing.assert_allclose(np.abs(a), np.abs(b), rtol=0, atol=1e-6)


def test_compute_pcs_empty_input_returns_empty_array(
    sklearn_module,  # noqa: ARG001
) -> None:
    matrix = np.zeros((0, 8), dtype=np.float32)
    pcs = compute_pcs(matrix, n_components=4)
    assert pcs.shape == (0, 0)


# --- SQLite I/O ------------------------------------------------------------


def test_read_embeddings_returns_only_done_rows_for_model(
    db: sqlite3.Connection,
) -> None:
    db.execute(
        "INSERT INTO track_embeddings VALUES (?,?,?,?,?,?,?,?)",
        ("t1", "m1", 3, _pack_vector(np.array([1, 2, 3], np.float32)),
         "done", None, 0, 0),
    )
    # different model — must be filtered out
    db.execute(
        "INSERT INTO track_embeddings VALUES (?,?,?,?,?,?,?,?)",
        ("t2", "m2", 3, _pack_vector(np.array([4, 5, 6], np.float32)),
         "done", None, 0, 0),
    )
    # not yet done — must be filtered out even though the model matches
    db.execute(
        "INSERT INTO track_embeddings VALUES (?,?,?,?,?,?,?,?)",
        ("t3", "m1", 0, None, "not_started", None, 0, 0),
    )

    got = read_embeddings_from_sqlite(db, "m1")
    assert [e.track_id for e in got] == ["t1"]
    np.testing.assert_array_equal(got[0].vector, np.array([1, 2, 3], np.float32))


def test_read_embeddings_is_empty_when_no_match(db: sqlite3.Connection) -> None:
    assert read_embeddings_from_sqlite(db, "nonexistent") == []


def test_write_projections_upserts_idempotently(db: sqlite3.Connection) -> None:
    written_first = write_projections_to_sqlite(
        db, "m1", "pv1", [Projection2D("t1", 0.0, 0.0)], now_ms=1000,
    )
    assert written_first == 1
    # Re-write with updated coords. The (track_id, model_version,
    # proj_version) PK + ON CONFLICT clause must replace, not append.
    written_second = write_projections_to_sqlite(
        db, "m1", "pv1", [Projection2D("t1", 9.0, 9.0)], now_ms=2000,
    )
    assert written_second == 1
    row = db.execute(
        "SELECT x, y, created_at_ms FROM embedding_projection_2d "
        "WHERE track_id = 't1'"
    ).fetchone()
    assert row == (9.0, 9.0, 2000)


def test_write_projections_uses_default_now_when_unspecified(
    db: sqlite3.Connection,
) -> None:
    before = int(time.time() * 1000)
    write_projections_to_sqlite(
        db, "m1", "pv1", [Projection2D("t1", 1.0, 2.0)],
    )
    after = int(time.time() * 1000)
    (ts,) = db.execute(
        "SELECT created_at_ms FROM embedding_projection_2d WHERE track_id='t1'"
    ).fetchone()
    assert before <= ts <= after


def test_write_projections_handles_empty_input(db: sqlite3.Connection) -> None:
    n = write_projections_to_sqlite(db, "m1", "pv1", [])
    assert n == 0
    rows = db.execute("SELECT COUNT(*) FROM embedding_projection_2d").fetchone()
    assert rows == (0,)


def test_write_projections_persists_pc_columns(db: sqlite3.Connection) -> None:
    # PCs are nullable: the writer accepts any subset, omits stay null.
    write_projections_to_sqlite(
        db,
        "m1",
        "pv1",
        [Projection2D("t1", 0.0, 0.0, pc1=1.5, pc2=-2.5, pc3=0.25, pc4=None)],
        now_ms=1000,
    )
    row = db.execute(
        "SELECT pc1, pc2, pc3, pc4 FROM embedding_projection_2d "
        "WHERE track_id = 't1'"
    ).fetchone()
    assert row == (1.5, -2.5, 0.25, None)


def test_write_projections_persists_z_column(db: sqlite3.Connection) -> None:
    # `z` is the third UMAP axis on a 3D run; nullable so 2D projections
    # leave it None and the colour-by dropdown can ignore the channel.
    write_projections_to_sqlite(
        db,
        "m1",
        "pv1",
        [Projection2D("t1", 0.0, 0.0, z=1.25)],
        now_ms=1000,
    )
    (z,) = db.execute(
        "SELECT z FROM embedding_projection_2d WHERE track_id = 't1'"
    ).fetchone()
    assert z == 1.25


def test_write_projections_leaves_z_null_by_default(db: sqlite3.Connection) -> None:
    write_projections_to_sqlite(
        db, "m1", "pv1", [Projection2D("t1", 0.0, 0.0)], now_ms=1000,
    )
    (z,) = db.execute(
        "SELECT z FROM embedding_projection_2d WHERE track_id = 't1'"
    ).fetchone()
    assert z is None


def test_write_projections_handles_all_null_pcs(db: sqlite3.Connection) -> None:
    # Default Projection2D leaves all PCs None — a backward-compat path
    # for any caller that hasn't been updated to compute them yet.
    write_projections_to_sqlite(
        db, "m1", "pv1", [Projection2D("t1", 0.0, 0.0)], now_ms=1000,
    )
    row = db.execute(
        "SELECT pc1, pc2, pc3, pc4 FROM embedding_projection_2d "
        "WHERE track_id = 't1'"
    ).fetchone()
    assert row == (None, None, None, None)


def test_write_projections_round_trips_two_proj_versions(
    db: sqlite3.Connection,
) -> None:
    # Different proj_versions on the same track_id must coexist.
    write_projections_to_sqlite(
        db, "m1", "pv1", [Projection2D("t1", 0.0, 0.0)],
    )
    write_projections_to_sqlite(
        db, "m1", "pv2", [Projection2D("t1", 1.0, 1.0)],
    )
    rows = db.execute(
        "SELECT proj_version, x FROM embedding_projection_2d "
        "WHERE track_id = 't1' ORDER BY proj_version"
    ).fetchall()
    assert rows == [("pv1", 0.0), ("pv2", 1.0)]


# --- end-to-end (requires umap-learn) -------------------------------------


@pytest.fixture
def umap_module():
    """Skip-or-import. Lets the rest of the suite stay green on a slim
    install while gating the run() test on the `reduce` extra."""
    return pytest.importorskip(
        "umap",
        reason="install with `uv sync --extra reduce` to run UMAP-backed tests",
    )


def test_run_projects_real_data_and_persists(
    db: sqlite3.Connection,
    tmp_path: Path,
    umap_module,  # noqa: ARG001 — fixture is the gate
) -> None:
    # 30 points: enough for UMAP's default n_neighbors=15, small enough
    # to run in well under a second.
    rng = np.random.default_rng(0)
    for i in range(30):
        v = rng.standard_normal(8).astype(np.float32)
        v /= np.linalg.norm(v)
        db.execute(
            "INSERT INTO track_embeddings VALUES (?,?,?,?,?,?,?,?)",
            (f"t{i:02d}", "m1", 8, _pack_vector(v), "done", None, 0, 0),
        )
    db.commit()
    db.close()

    db_path = tmp_path / "rec.sqlite"
    # Re-open as a path-based connection so `run()` can manage its own
    # lifecycle (it opens and closes the connection internally).
    pv, written = run(
        db_path=db_path,
        model_version="m1",
        n_neighbors=10,  # < 30 points; UMAP requires k < n
        min_dist=0.1,
        random_state=42,
    )
    assert pv == "umap-v1-rs42-n10-m0p10"
    assert written == 30

    # Read back: 30 rows, finite coordinates.
    conn = sqlite3.connect(db_path)
    rows = conn.execute(
        "SELECT x, y FROM embedding_projection_2d WHERE proj_version = ?",
        (pv,),
    ).fetchall()
    assert len(rows) == 30
    for x, y in rows:
        assert np.isfinite(x)
        assert np.isfinite(y)


def test_run_writes_z_for_3d_projection(
    db: sqlite3.Connection,
    tmp_path: Path,
    umap_module,  # noqa: ARG001 — fixture is the gate
) -> None:
    # Smaller dataset than the 2D test — UMAP-3D produces a 30×3 layout,
    # and we just need to verify the `z` column populates end-to-end.
    rng = np.random.default_rng(0)
    for i in range(30):
        v = rng.standard_normal(8).astype(np.float32)
        v /= np.linalg.norm(v)
        db.execute(
            "INSERT INTO track_embeddings VALUES (?,?,?,?,?,?,?,?)",
            (f"t{i:02d}", "m1", 8, _pack_vector(v), "done", None, 0, 0),
        )
    db.commit()
    db.close()

    db_path = tmp_path / "rec.sqlite"
    pv, written = run(
        db_path=db_path,
        model_version="m1",
        n_neighbors=10,
        min_dist=0.1,
        random_state=42,
        n_components=3,
    )
    assert pv == "umap-v1-rs42-n10-m0p10-d3"
    assert written == 30

    conn = sqlite3.connect(db_path)
    rows = conn.execute(
        "SELECT x, y, z FROM embedding_projection_2d WHERE proj_version = ?",
        (pv,),
    ).fetchall()
    assert len(rows) == 30
    for x, y, z in rows:
        assert np.isfinite(x)
        assert np.isfinite(y)
        assert z is not None and np.isfinite(z)


def test_run_returns_zero_when_no_embeddings(
    db: sqlite3.Connection, tmp_path: Path,
) -> None:
    db.close()
    pv, written = run(
        db_path=tmp_path / "rec.sqlite",
        model_version="nonexistent-model",
        random_state=42,
    )
    assert written == 0
    assert pv == default_proj_version(random_state=42)
