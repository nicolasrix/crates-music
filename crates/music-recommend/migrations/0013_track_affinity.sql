-- Per-track user-preference affinity for the recommender.
--
-- Why a dedicated table (vs. deriving from `events`/`recommend_feedback`
-- on read): the recommend hot path scores ~80 candidates per request and
-- needs an O(1) per-candidate affinity lookup. Aggregating the event log
-- + feedback table per candidate, per request, would dominate latency —
-- the same reasoning that gave `play_history` its own table (see
-- 0004_play_history.sql). This is the precomputed view.
--
-- Why a single decayed `score` column (vs. raw counts decayed on read):
-- taste drifts, so old signal must fade. Storing a decayed counter
-- `(score, updated_ms)` lets us fold a new event in O(1) — decay the
-- stored score to the event time, add the event weight — and decay once
-- more to "now" at read time. Exponential decay is composable, so no
-- nightly batch recompute is needed. See `preference::fold_event`.
--
-- The raw lifetime tallies (`play_count`, `skip_count`, `like_count`,
-- `dislike_count`) are NOT used in scoring; they exist purely so the
-- diagnostics surface can explain *why* a track's affinity is what it is
-- ("disliked 3×, skipped 5×") without reconstructing it from the log.
--
-- This table is a recommender-internal derived index. Navidrome remains
-- the canonical play-count ledger; the append-only `events` +
-- `recommend_feedback` tables remain the durable signal of record. If
-- this table is lost in a restore, it can be rebuilt by replaying them.
--
-- Why WITHOUT ROWID: track_id is the natural primary key and access is
-- point lookups + upserts only — the SQLite-recommended layout here.
CREATE TABLE track_affinity (
    track_id       TEXT    PRIMARY KEY,
    -- Exponentially-decayed net affinity, anchored at `updated_ms`.
    score          REAL    NOT NULL DEFAULT 0,
    -- Wall-clock ms the score was last decayed/updated to.
    updated_ms     INTEGER NOT NULL DEFAULT 0,
    -- Raw lifetime tallies for diagnostics only (not scored).
    play_count     INTEGER NOT NULL DEFAULT 0,
    skip_count     INTEGER NOT NULL DEFAULT 0,
    like_count     INTEGER NOT NULL DEFAULT 0,
    dislike_count  INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;
