-- Per-(track, session) recommendation feedback.
--
-- Surface: thumb-up / thumb-down captured in the player. Semantically
-- a *recommendation* vote — "was this a good fit to play right now?" —
-- not a track-level rating (Subsonic's `starred` already covers that).
--
-- Why UNIQUE(track_id, session_id) with UPSERT semantics: users flip
-- their minds (thumb-up → thumb-down) and re-press to clear (handled
-- at the gateway as a DELETE; the row is removed rather than recorded
-- as a third vote-state). Per-session uniqueness keeps the "one vote
-- per session per track" mental model clean without dragging in a
-- user_id (single-tenant gateway).
--
-- vote ∈ {-1, +1}: integer because it's the natural shape for
-- aggregations (SUM gives net score; COUNTs by sign give up/down
-- counts). CHECK constraint pins the domain so a bad insert is
-- visible at write-time.
--
-- received_ms is gateway-stamped (trustable for chronology); occurred_ms
-- is client-stamped (matches the listening session). Keeping both lets
-- the diagnostics UI sort "most recent feedback" without relying on
-- client clocks.
CREATE TABLE recommend_feedback (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    track_id     TEXT    NOT NULL,
    session_id   TEXT    NOT NULL,
    vote         INTEGER NOT NULL CHECK (vote IN (-1, 1)),
    occurred_ms  INTEGER NOT NULL,
    received_ms  INTEGER NOT NULL,
    UNIQUE (track_id, session_id)
);

CREATE INDEX recommend_feedback_track ON recommend_feedback (track_id);
CREATE INDEX recommend_feedback_received ON recommend_feedback (received_ms);
