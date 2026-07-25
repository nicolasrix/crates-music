-- Per-user partition of the recency clock (multi-user, PR E).
--
-- `play_history` is WITHOUT ROWID with `track_id` as the primary key, so
-- "user A and user B each played track X at different times" needs the
-- key to widen to `(user_id, track_id)`. SQLite can't alter a primary
-- key in place, so recreate the table and copy rows across — the same
-- create/copy/drop/rename dance migration 0015 used.
--
-- DEFAULT 1 attributes every existing row to the owner. user_id is a
-- plain integer mirroring `users.id` (no cross-file FK; see 0017).
CREATE TABLE play_history_new (
    user_id        INTEGER NOT NULL DEFAULT 1,
    track_id       TEXT    NOT NULL,
    last_played_ms INTEGER NOT NULL,
    PRIMARY KEY (user_id, track_id)
) WITHOUT ROWID;

INSERT INTO play_history_new (user_id, track_id, last_played_ms)
    SELECT 1, track_id, last_played_ms FROM play_history;

DROP TABLE play_history;
ALTER TABLE play_history_new RENAME TO play_history;
