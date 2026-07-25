-- Per-user partition of the decayed preference-affinity counter
-- (multi-user, PR E).
--
-- WITHOUT ROWID with `track_id` as the primary key, so the key widens to
-- `(user_id, track_id)` — each user folds their own plays/skips/thumbs
-- into their own counter. SQLite can't alter a primary key in place;
-- recreate + copy (cf. 0015, 0018).
--
-- DEFAULT 1 attributes every existing row to the owner. user_id is a
-- plain integer mirroring `users.id` (no cross-file FK; see 0017).
CREATE TABLE track_affinity_new (
    user_id        INTEGER NOT NULL DEFAULT 1,
    track_id       TEXT    NOT NULL,
    score          REAL    NOT NULL DEFAULT 0,
    updated_ms     INTEGER NOT NULL DEFAULT 0,
    play_count     INTEGER NOT NULL DEFAULT 0,
    skip_count     INTEGER NOT NULL DEFAULT 0,
    like_count     INTEGER NOT NULL DEFAULT 0,
    dislike_count  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (user_id, track_id)
) WITHOUT ROWID;

INSERT INTO track_affinity_new
        (user_id, track_id, score, updated_ms,
         play_count, skip_count, like_count, dislike_count)
    SELECT 1, track_id, score, updated_ms,
           play_count, skip_count, like_count, dislike_count
        FROM track_affinity;

DROP TABLE track_affinity;
ALTER TABLE track_affinity_new RENAME TO track_affinity;
