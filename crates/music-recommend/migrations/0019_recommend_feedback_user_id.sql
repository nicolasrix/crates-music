-- Per-user partition of recommendation thumb votes (multi-user, PR E).
--
-- The existing UNIQUE (track_id, session_id) already isolates by user in
-- practice — a recommend `session_id` is a globally-unique uuid-v7 owned
-- by exactly one user — so the uniqueness constraint stays as-is and a
-- plain ADD COLUMN suffices (no table recreate). The new column lets the
-- aggregate `for_track` count be scoped to the asking user and stamps
-- each vote with its author for future per-user training.
--
-- DEFAULT 1 backfills existing rows to the owner. user_id is a plain
-- indexed integer mirroring `users.id` (no cross-file FK; see 0017).
ALTER TABLE recommend_feedback ADD COLUMN user_id INTEGER NOT NULL DEFAULT 1;

CREATE INDEX recommend_feedback_user_id_idx ON recommend_feedback (user_id);
