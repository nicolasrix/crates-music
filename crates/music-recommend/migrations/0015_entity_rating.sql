-- Generalize per-track like/dislike (0014) to any rateable library entity:
-- tracks, albums, and artists. The product semantics are identical to the
-- track case — a durable, explicit, NON-decaying verdict the gateway owns
-- outright and never writes back to Navidrome — only the subject widens:
--
--   kind = 'track'   → the song itself          (as in 0014)
--   kind = 'album'   → every track on the album
--   kind = 'artist'  → every track by the artist
--
-- rating semantics, unchanged from 0014:
--   rating = +1  → like     (boosts recommendation relevance; surfaced on
--                            the "Liked" page. Track contributes most, then
--                            album, then artist — see LIKE_BONUS* consts.)
--   rating = -1  → dislike   (the entity is excluded from play entirely:
--                            its tracks are hard-excluded from every
--                            recommender candidate path and auto-skipped on
--                            queue advance in the web player)
--   no row       → neutral   (clearing a rating deletes the row)
--
-- This SUPERSEDES `track_rating` (0014). The migration is forward-only,
-- consistent with the rest of the set: it copies the existing rows across
-- as `kind = 'track'` before dropping the old table, so no verdict is lost.
-- (Rolling the gateway binary back across 0015 would strip the `kind`
-- column and need the copy reversed — not a concern for normal forward
-- deploys, but noted here for the record.)
--
-- WITHOUT ROWID with the composite (kind, entity_id) primary key: access is
-- point lookups + upserts + small `WHERE kind = ? AND rating = ?` scans —
-- the SQLite-recommended layout, same rationale as track_rating /
-- track_affinity. entity_id is generic TEXT because album and artist ids
-- are not TrackId; the kind column disambiguates the namespace.
CREATE TABLE entity_rating (
    kind       TEXT    NOT NULL CHECK (kind IN ('track', 'album', 'artist')),
    entity_id  TEXT    NOT NULL,
    rating     INTEGER NOT NULL CHECK (rating IN (-1, 1)),  -- +1 like / -1 dislike
    updated_ms INTEGER NOT NULL,
    PRIMARY KEY (kind, entity_id)
) WITHOUT ROWID;

-- Carry the live track ratings (0014) across before dropping the old table.
INSERT INTO entity_rating (kind, entity_id, rating, updated_ms)
    SELECT 'track', track_id, rating, updated_ms FROM track_rating;

DROP TABLE track_rating;
