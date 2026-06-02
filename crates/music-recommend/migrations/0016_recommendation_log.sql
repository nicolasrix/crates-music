-- Recommendation provenance log: what the recommender served, in what
-- context, with what scores — the training substrate for future
-- learning-to-rank / supervised-metric models.
--
-- Today the feedback signal (events: skip/scrobble, recommend_feedback:
-- thumb votes, entity_rating: like/dislike) records *what the user did*
-- but NOT *what was recommended to them and in what order*. Without that
-- join, you can mine session co-occurrence but you cannot learn from the
-- recommender's own errors (the served-but-skipped vs served-and-kept
-- distinction). These two tables close that loop.
--
-- Two-table normalised shape, mirroring the request → slate structure:
--   * recommendation       — one row per served request (the context)
--   * recommendation_item   — one row per served candidate (the slate)
--
-- The outcome join is done at training time, NOT here: link
-- recommendation_item.entity_id → events.track_id where
-- events.occurred_at > recommendation.served_ms (optionally scoped by
-- session_id). Keeping outcomes out of this table preserves the
-- append-only, write-once-at-serve-time property — provenance is a
-- faithful snapshot of what was decided, never mutated after the fact.
--
-- No automatic trimming: this is deliberately durable training data, not
-- a diagnostics ring. Growth is modest at single-user scale (tens of
-- requests/day × ~20 items). The served_ms index makes time-range
-- training queries cheap and any future retention sweep trivial.

CREATE TABLE recommendation (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    -- next | station | from_seeds | from_any | similar_albums | similar_artists
    kind          TEXT    NOT NULL,
    -- Recommend-session this request belonged to, when the caller stamped
    -- one (from_seeds / from_any carry it; next / station / similar_* do
    -- not). NULL = no session; outcomes still join by track_id + time.
    session_id    TEXT,
    -- Embedding model the ANN + whitening were built from. CRITICAL: a
    -- model bump re-embeds everything, so scores are only comparable
    -- within a model_version. Training data MUST be version-stamped.
    model_version TEXT    NOT NULL,
    -- JSON array of the seed/candidate track ids the request was built
    -- from. NULL for the text station (no track seeds).
    seeds_json    TEXT,
    -- Natural-language station query. NULL for all seed-based paths.
    text_query    TEXT,
    -- JSON object of the knobs in force for this request: n, per_seed_n,
    -- sample_size, leash {active, tau, lambda, anchors}, diversity
    -- {mode, mmr_lambda, artist_penalty}, queue/exclude sizes, etc.
    -- Opaque on purpose — the set of knobs evolves; the blob does not
    -- force a migration each time (mirrors events.metadata).
    params_json   TEXT    NOT NULL DEFAULT '{}',
    -- The recommender ran in degraded mode (e.g. no seed was embedded).
    degraded      INTEGER NOT NULL DEFAULT 0,
    -- Number of items served (denormalised; = COUNT(recommendation_item)).
    result_count  INTEGER NOT NULL,
    -- Gateway-stamped serve time (unix ms). Trustable clock for the
    -- outcome join; client clocks are not.
    served_ms     INTEGER NOT NULL
);

CREATE INDEX recommendation_served_ms_idx ON recommendation (served_ms);
CREATE INDEX recommendation_session_idx   ON recommendation (session_id);
CREATE INDEX recommendation_kind_idx      ON recommendation (kind);

CREATE TABLE recommendation_item (
    recommendation_id INTEGER NOT NULL REFERENCES recommendation(id) ON DELETE CASCADE,
    -- 0-based position in the served slate. The label of record for
    -- position/presentation bias: a skip at rank 0 ≠ a skip at rank 18.
    rank              INTEGER NOT NULL,
    -- track_id for next/station/from_seeds/from_any; album_id / artist_id
    -- for the similar_* endpoints (the parent `kind` disambiguates).
    entity_id         TEXT    NOT NULL,
    -- The final scalar the recommender ordered by (post bonus / leash /
    -- filter). NULL only if an endpoint exposes no scalar.
    score             REAL,
    -- JSON object of per-candidate features computed at serve time:
    -- similarity, seed_hits, affinity_bonus, supporting_tracks, etc.
    -- These are the model's input features at the moment of the decision —
    -- capturing them avoids re-deriving from tables that drift (affinity
    -- decays, ratings change) by training time.
    features_json     TEXT    NOT NULL DEFAULT '{}',
    PRIMARY KEY (recommendation_id, rank)
) WITHOUT ROWID;

CREATE INDEX recommendation_item_entity_idx ON recommendation_item (entity_id);
