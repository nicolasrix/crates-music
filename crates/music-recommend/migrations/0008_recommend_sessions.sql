-- Durable record of recommend-session lifetimes. One row per
-- SyncOp::StartSession. The in-memory `PlaybackState.session_anchor`
-- is a *view* of the currently-open row (ended_ms IS NULL); this table
-- is the persisted source of truth that survives gateway restarts.
--
-- ended_ms is NULL while the session is live. It gets stamped on:
--   * SyncOp::StopSession (explicit close), or
--   * The next SyncOp::StartSession (implicit close at the new
--     started_ms — sync state allows exactly one active session at
--     a time, so opening a new one closes the old).
--
-- A row may stay with ended_ms IS NULL forever if the gateway dies
-- mid-session. We accept that as ground truth ("we don't know when
-- this session ended") rather than synthesise a fake endpoint. The
-- next StartSession after restart implicitly closes any orphan.
--
-- anchor_track_id + items_count are the user-intent fingerprint at
-- session start. Stored alongside the lifecycle so reconstruction
-- doesn't need to cross-reference the events log.
CREATE TABLE recommend_sessions (
    session_id      TEXT    PRIMARY KEY,
    anchor_track_id TEXT    NOT NULL,
    items_count     INTEGER NOT NULL,
    started_ms      INTEGER NOT NULL,
    ended_ms        INTEGER          -- NULL = still active
);

CREATE INDEX recommend_sessions_started_idx ON recommend_sessions (started_ms);
-- Partial index gives O(1) "is there an active session right now?"
-- — useful for the gateway boot path to detect orphan sessions.
CREATE INDEX recommend_sessions_active_idx  ON recommend_sessions (ended_ms) WHERE ended_ms IS NULL;
