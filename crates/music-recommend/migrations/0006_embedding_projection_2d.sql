-- 2D UMAP projection of `track_embeddings.vector`.
--
-- Cache, not source of truth: rebuildable from the embeddings + a
-- fixed UMAP config any time. We persist it so the diagnostics page
-- can render the latent-space scatter without re-running UMAP on
-- every request (a few seconds at our scale, but unbounded as the
-- catalog grows; recomputing on read would also drift the layout
-- between page loads since UMAP is stochastic).
--
-- Why `proj_version` is part of the PK: UMAP outputs depend on the
-- algorithm parameters (n_neighbors, min_dist, random_state) AND the
-- embedding-set composition. We encode the param choice into a
-- short string (e.g. `umap-v1-rs42-n15-m0p1`) so multiple coexisting
-- projections — one to compare against another — are addressable
-- without dropping rows.
--
-- Why model_version is in the PK alongside track_id: an embedding is
-- per-(track, model), and a projection sits on top of those. A model
-- swap produces a new embedding set, which produces a new projection
-- set, and we want both to coexist for diffability.
--
-- WITHOUT ROWID: composite PK + point lookups + batch upserts. No
-- range scans on rowid.

CREATE TABLE embedding_projection_2d (
    track_id        TEXT    NOT NULL,
    model_version   TEXT    NOT NULL,
    proj_version    TEXT    NOT NULL,
    x               REAL    NOT NULL,
    y               REAL    NOT NULL,
    created_at_ms   INTEGER NOT NULL,
    PRIMARY KEY (track_id, model_version, proj_version)
) WITHOUT ROWID;

-- Fetch-by-projection is the diagnostics endpoint's hot path
-- ("give me all points for this proj_version").
CREATE INDEX embedding_projection_2d_proj
    ON embedding_projection_2d (proj_version, model_version);
