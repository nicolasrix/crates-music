-- Durable per-track like/dislike — the user's explicit taste signal.
--
-- This is deliberately a SEPARATE channel from `track_affinity`
-- (0013): that is a decayed counter folding implicit signal (plays,
-- skips) plus the session-scoped recommendation thumbs, and it FADES
-- over time. A like/dislike here is an explicit, durable verdict on the
-- *song itself* and must NOT decay — a track you disliked last year is
-- still disliked until you say otherwise. Keeping them apart also avoids
-- double-counting: folding a like into the decaying counter would
-- duplicate the thumb-up path.
--
-- Semantics:
--   rating = +1  → like     (boosts recommendation relevance; "Liked songs")
--   rating = -1  → dislike   (hard-excluded from all recommender candidates,
--                             auto-skipped on queue advance in the web player)
--   no row       → neutral   (clearing a rating deletes the row)
--
-- The gateway owns this taste store outright; it is NEVER written back to
-- Navidrome (Navidrome stays a read-only catalog). A future opt-in export
-- could map likes → Subsonic `star`, but that is the only place a write
-- would ever happen.
--
-- WITHOUT ROWID: track_id is the natural key and access is point
-- lookups + upserts only — the SQLite-recommended layout, same as
-- track_affinity.
CREATE TABLE track_rating (
    track_id   TEXT    PRIMARY KEY,
    rating     INTEGER NOT NULL CHECK (rating IN (-1, 1)),  -- +1 like / -1 dislike
    updated_ms INTEGER NOT NULL
) WITHOUT ROWID;
