-- Per-user partition of durable like/dislike verdicts (multi-user, PR E).
--
-- WITHOUT ROWID with PRIMARY KEY (kind, entity_id), so the key widens to
-- (user_id, kind, entity_id) — user A liking a track is invisible to
-- user B, and a guest's verdicts (gateway-side, never written) can't
-- reshape the host's taste. SQLite can't alter a primary key in place;
-- recreate + copy (cf. 0015, which itself created this table).
--
-- DEFAULT 1 attributes every existing verdict to the owner. user_id is a
-- plain integer mirroring `users.id` (no cross-file FK; see 0017).
CREATE TABLE entity_rating_new (
    user_id    INTEGER NOT NULL DEFAULT 1,
    kind       TEXT    NOT NULL CHECK (kind IN ('track', 'album', 'artist')),
    entity_id  TEXT    NOT NULL,
    rating     INTEGER NOT NULL CHECK (rating IN (-1, 1)),  -- +1 like / -1 dislike
    updated_ms INTEGER NOT NULL,
    PRIMARY KEY (user_id, kind, entity_id)
) WITHOUT ROWID;

INSERT INTO entity_rating_new (user_id, kind, entity_id, rating, updated_ms)
    SELECT 1, kind, entity_id, rating, updated_ms FROM entity_rating;

DROP TABLE entity_rating;
ALTER TABLE entity_rating_new RENAME TO entity_rating;
