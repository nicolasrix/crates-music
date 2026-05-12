"""UMAP 2-D reduction of CLAP embeddings, for the latent-space view on
the diagnostics page.

Run as a one-shot CLI:

    uv run --extra reduce python -m embedder.reduce \\
        --db /path/to/gateway-state.recommend.sqlite \\
        --model-version music_audioset_epoch_15_esc_90.14

The script is a *batch maintenance* job — not part of the FastAPI
service runtime. The embedder image stays slim by default; UMAP and
its numba/scipy dependency tree only pull in under the `reduce` extra
in `pyproject.toml`.

Reproducibility:
  - `random_state` is pinned (default 42). UMAP's seed plumbing is
    well-behaved with a fixed `random_state` *as long as* `n_jobs=1`.
    With multi-threading enabled, numba-jitted paths inside umap-learn
    introduce nondeterminism even with the seed pinned. We force
    `n_jobs=1` for that reason.
  - The `proj_version` string encodes the active params, so multiple
    differently-parameterised projections can coexist in SQLite for
    A/B comparison.
"""

from __future__ import annotations

import argparse
import logging
import sqlite3
import struct
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Sequence

import numpy as np

logger = logging.getLogger(__name__)


# Default UMAP knobs. Cosine metric matches the geometry CLAP was
# trained with — euclidean on L2-normalized vectors degenerates to
# cosine-equivalence but cosine is the literal contract.
DEFAULT_N_NEIGHBORS = 15
DEFAULT_MIN_DIST = 0.1
DEFAULT_RANDOM_STATE = 42
DEFAULT_METRIC = "cosine"


@dataclass(frozen=True)
class Embedding:
    """One row of (track_id, model_version, vector). Frozen so the
    callers can't mutate vectors in place."""

    track_id: str
    model_version: str
    vector: np.ndarray  # shape (D,) float32


@dataclass(frozen=True)
class Projection2D:
    """One row of `(track_id, x, y, pc1..pc4, z)`. UMAP supplies
    `(x, y)`; PCA on the same input matrix supplies the four PCs. `z`
    is the third axis of a 3-component UMAP run — None for 2D runs.

    The class name keeps `2D` for backward compatibility with the
    schema; the persistence shape is "2D coordinates plus optional
    continuous channels (PCs, z) feeding the colour dropdown."
    """

    track_id: str
    x: float
    y: float
    pc1: float | None = None
    pc2: float | None = None
    pc3: float | None = None
    pc4: float | None = None
    z: float | None = None


def default_proj_version(
    n_neighbors: int = DEFAULT_N_NEIGHBORS,
    min_dist: float = DEFAULT_MIN_DIST,
    random_state: int = DEFAULT_RANDOM_STATE,
    n_components: int = 2,
) -> str:
    """Stable, human-readable identifier for "the UMAP run with these
    params on this algorithm version." Used as the `proj_version`
    column when the caller doesn't supply one.

    The `v1` prefix bumps if the algorithm itself changes (e.g. swap
    UMAP for PaCMAP); the parameter suffix encodes the knobs so two
    coexisting runs with different params are uniquely keyed.

    `n_components` is restricted to {2, 3}: 2 leaves the suffix off so
    existing names stay stable, 3 appends `-d3` so a 3D run lives
    alongside its 2D sibling under the same param set.
    """
    if n_components not in (2, 3):
        raise ValueError(
            f"n_components must be 2 or 3 (got {n_components}); the "
            f"persistence schema only encodes (x, y) plus optional z."
        )
    # `.` is special in some shells / URLs; lower it to `p` so the
    # version string is safe to pass on a query string.
    min_dist_token = f"{min_dist:.2f}".replace(".", "p")
    base = f"umap-v1-rs{random_state}-n{n_neighbors}-m{min_dist_token}"
    return f"{base}-d3" if n_components == 3 else base


def decode_vector_blob(blob: bytes, dim: int) -> np.ndarray:
    """Inverse of the Rust side's `bincode`-free little-endian f32
    packing — `vector` in `track_embeddings` is `dim * 4` bytes of
    little-endian f32. Returns a 1-D numpy array.

    Raises `ValueError` if the blob length doesn't match `dim * 4`,
    so a corrupt row surfaces immediately rather than as silently
    bad coordinates.
    """
    expected = dim * 4
    if len(blob) != expected:
        raise ValueError(
            f"vector blob length {len(blob)} does not match dim*4 ({expected})"
        )
    # struct.unpack with the explicit endian byte is portable.
    floats = struct.unpack(f"<{dim}f", blob)
    return np.asarray(floats, dtype=np.float32)


def read_embeddings_from_sqlite(
    conn: sqlite3.Connection,
    model_version: str,
) -> list[Embedding]:
    """All embedded rows for the given `model_version`. Status = 'done'
    only — partial rows have nothing useful to project.

    Returns an empty list when no rows match (caller decides whether
    that's an error)."""
    rows = conn.execute(
        """SELECT track_id, dim, vector
           FROM track_embeddings
           WHERE model_version = ? AND status = 'done' AND vector IS NOT NULL
           ORDER BY track_id""",
        (model_version,),
    ).fetchall()

    out: list[Embedding] = []
    for track_id, dim, blob in rows:
        out.append(
            Embedding(
                track_id=track_id,
                model_version=model_version,
                vector=decode_vector_blob(blob, dim),
            )
        )
    return out


def compute_pcs(matrix: np.ndarray, n_components: int = 4) -> np.ndarray:
    """Run PCA on the input matrix and return the first `n_components`
    principal-component scores per row.

    Clamps `n_components` to `min(n_components, N, D)` so tiny dev
    datasets (e.g. 3 points, or vectors of dim < 4) don't crash. The
    caller is expected to pad missing columns with `None` when
    persisting — see how `project_embeddings` consumes this.

    Same inputs yield identical outputs to within FP rounding (PCA is
    a closed-form SVD; no RNG involved). PC signs are arbitrary by
    construction — don't rely on the sign of any one column.

    Returns an empty `(0, 0)` array for an empty input matrix, matching
    the convention `project_embeddings` uses for the UMAP step.
    """
    n_rows = matrix.shape[0]
    n_dims = matrix.shape[1] if matrix.ndim == 2 else 0
    if n_rows == 0 or n_dims == 0:
        return np.zeros((0, 0), dtype=np.float32)
    # sklearn.decomposition.PCA rejects n_components > min(N, D); clamp
    # ourselves so the surface is "PCA with whatever fits" rather than
    # an exception the caller has to catch.
    k = max(1, min(n_components, n_rows, n_dims))
    # Defer the import for the same reason as UMAP — sklearn is heavy.
    from sklearn.decomposition import PCA  # type: ignore[import-untyped]

    pca = PCA(n_components=k)
    return pca.fit_transform(matrix).astype(np.float32)


def project_embeddings(
    embeddings: Sequence[Embedding],
    *,
    n_neighbors: int = DEFAULT_N_NEIGHBORS,
    min_dist: float = DEFAULT_MIN_DIST,
    random_state: int = DEFAULT_RANDOM_STATE,
    metric: str = DEFAULT_METRIC,
    pc_components: int = 4,
    n_components: int = 2,
) -> list[Projection2D]:
    """Run UMAP on the given embeddings and return one `Projection2D`
    per input row in the same order.

    This is a *pure* function: same inputs + same params yield the
    same outputs (modulo BLAS-level FP rounding, which we ignore). The
    SQLite I/O happens in `read_embeddings_from_sqlite` /
    `write_projections_to_sqlite` so the algorithm is testable in
    isolation.

    `n_neighbors` must be < the number of points; UMAP raises a clearer
    error than we can, so we let it propagate.
    """
    if len(embeddings) == 0:
        return []
    if n_components not in (2, 3):
        raise ValueError(
            f"n_components must be 2 or 3 (got {n_components}); only "
            f"(x, y) and (x, y, z) layouts are supported."
        )

    # Defer the import. `umap-learn` pulls in numba + scipy + sklearn,
    # ~150 MB of extra wheels. Keeping the import here means the
    # `Embedding` / `Projection2D` types and `default_proj_version`
    # remain importable from the slim baseline install.
    import umap  # type: ignore[import-untyped]

    matrix = np.stack([e.vector for e in embeddings]).astype(np.float32)

    # n_jobs=1: the only safe setting when we want reproducibility
    # under a pinned random_state. umap-learn parallelises with numba
    # by default, which races on the RNG state inside the optimisation
    # loop. The wall-clock penalty at our scale (~10⁴ points) is
    # measured in seconds, not minutes, so determinism wins.
    reducer = umap.UMAP(
        n_components=n_components,
        n_neighbors=n_neighbors,
        min_dist=min_dist,
        metric=metric,
        random_state=random_state,
        n_jobs=1,
    )
    coords = reducer.fit_transform(matrix)  # shape (N, n_components)
    pcs = compute_pcs(matrix, n_components=pc_components)  # shape (N, k≤4)

    def _pick(i: int, j: int) -> float | None:
        # PCs are clamped to min(k, N, D); read defensively so a
        # narrower PCA result simply leaves later columns null.
        if j >= pcs.shape[1] or i >= pcs.shape[0]:
            return None
        return float(pcs[i, j])

    return [
        Projection2D(
            track_id=e.track_id,
            x=float(coords[i, 0]),
            y=float(coords[i, 1]),
            pc1=_pick(i, 0),
            pc2=_pick(i, 1),
            pc3=_pick(i, 2),
            pc4=_pick(i, 3),
            z=float(coords[i, 2]) if n_components == 3 else None,
        )
        for i, e in enumerate(embeddings)
    ]


def write_projections_to_sqlite(
    conn: sqlite3.Connection,
    model_version: str,
    proj_version: str,
    projections: Iterable[Projection2D],
    now_ms: int | None = None,
) -> int:
    """Idempotent upsert: rerunning with the same (track_id,
    model_version, proj_version) replaces the coords. Returns the
    number of rows written.

    Wraps the inserts in a single transaction. Without that, SQLite
    auto-commits each INSERT, which is ~100× slower at 5k rows.
    """
    if now_ms is None:
        now_ms = int(time.time() * 1000)
    rows = [
        (
            p.track_id,
            model_version,
            proj_version,
            p.x,
            p.y,
            now_ms,
            p.pc1,
            p.pc2,
            p.pc3,
            p.pc4,
            p.z,
        )
        for p in projections
    ]
    if not rows:
        return 0
    with conn:  # transactional
        conn.executemany(
            """INSERT INTO embedding_projection_2d
                   (track_id, model_version, proj_version,
                    x, y, created_at_ms,
                    pc1, pc2, pc3, pc4, z)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(track_id, model_version, proj_version) DO UPDATE SET
                   x = excluded.x,
                   y = excluded.y,
                   created_at_ms = excluded.created_at_ms,
                   pc1 = excluded.pc1,
                   pc2 = excluded.pc2,
                   pc3 = excluded.pc3,
                   pc4 = excluded.pc4,
                   z = excluded.z""",
            rows,
        )
    return len(rows)


def run(
    db_path: Path,
    model_version: str,
    *,
    n_neighbors: int = DEFAULT_N_NEIGHBORS,
    min_dist: float = DEFAULT_MIN_DIST,
    random_state: int = DEFAULT_RANDOM_STATE,
    proj_version: str | None = None,
    n_components: int = 2,
) -> tuple[str, int]:
    """End-to-end orchestrator: read → project → write. Returns the
    final (proj_version, row_count) so the CLI can log it and tests
    can assert on the round-trip.
    """
    pv = proj_version or default_proj_version(
        n_neighbors=n_neighbors,
        min_dist=min_dist,
        random_state=random_state,
        n_components=n_components,
    )
    with sqlite3.connect(db_path) as conn:
        embeddings = read_embeddings_from_sqlite(conn, model_version)
        logger.info(
            "read %d embeddings for model_version=%s",
            len(embeddings),
            model_version,
        )
        if not embeddings:
            return pv, 0
        projections = project_embeddings(
            embeddings,
            n_neighbors=n_neighbors,
            min_dist=min_dist,
            random_state=random_state,
            n_components=n_components,
        )
        written = write_projections_to_sqlite(conn, model_version, pv, projections)
    return pv, written


def _build_arg_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="embedder.reduce",
        description="Run UMAP to project track_embeddings into 2D.",
    )
    p.add_argument(
        "--db",
        type=Path,
        required=True,
        help="Path to gateway-state.recommend.sqlite",
    )
    p.add_argument(
        "--model-version",
        required=True,
        help="model_version of the embeddings to project (e.g. "
        "music_audioset_epoch_15_esc_90.14)",
    )
    p.add_argument(
        "--n-neighbors",
        type=int,
        default=DEFAULT_N_NEIGHBORS,
        help=f"UMAP n_neighbors (default {DEFAULT_N_NEIGHBORS})",
    )
    p.add_argument(
        "--min-dist",
        type=float,
        default=DEFAULT_MIN_DIST,
        help=f"UMAP min_dist (default {DEFAULT_MIN_DIST})",
    )
    p.add_argument(
        "--random-state",
        type=int,
        default=DEFAULT_RANDOM_STATE,
        help=f"UMAP random_state, pinned for reproducibility "
        f"(default {DEFAULT_RANDOM_STATE})",
    )
    p.add_argument(
        "--proj-version",
        default=None,
        help="Override the auto-derived proj_version string. Defaults "
        "to `umap-v1-rs<rs>-n<nn>-m<min_dist>[-d3]`.",
    )
    p.add_argument(
        "--n-components",
        type=int,
        choices=[2, 3],
        default=2,
        help="UMAP output dimensionality. 2 (default) populates only "
        "(x, y); 3 additionally writes `z`, exposed to the web "
        "colour-by dropdown as 'UMAP z'.",
    )
    p.add_argument(
        "-v",
        "--verbose",
        action="store_true",
        help="Enable INFO-level logging to stderr.",
    )
    return p


def main(argv: Sequence[str] | None = None) -> int:
    args = _build_arg_parser().parse_args(argv)
    logging.basicConfig(
        level=logging.INFO if args.verbose else logging.WARNING,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
        stream=sys.stderr,
    )
    pv, written = run(
        db_path=args.db,
        model_version=args.model_version,
        n_neighbors=args.n_neighbors,
        min_dist=args.min_dist,
        random_state=args.random_state,
        proj_version=args.proj_version,
        n_components=args.n_components,
    )
    if written == 0:
        logger.warning(
            "no embeddings found for model_version=%s; nothing written",
            args.model_version,
        )
        return 1
    logger.info("wrote %d projections under proj_version=%s", written, pv)
    # Always print the summary line to stdout so a shell pipeline can
    # consume the result regardless of log level.
    sys.stdout.write(f"{pv}\t{written}\n")
    return 0


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
