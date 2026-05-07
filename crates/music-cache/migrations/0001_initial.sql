-- L2 metadata cache: gateway-side ETag store for Subsonic browse responses.
-- Single table for now; the recommender, OAuth, and events log live in
-- separate SQLite files (different concerns, different write rates).

CREATE TABLE IF NOT EXISTS cache_entries (
    key          TEXT PRIMARY KEY NOT NULL,
    etag         TEXT NOT NULL,
    body         BLOB NOT NULL,
    fetched_at   INTEGER NOT NULL,  -- unix epoch seconds
    ttl_seconds  INTEGER NOT NULL CHECK (ttl_seconds >= 0)
);

CREATE INDEX IF NOT EXISTS cache_entries_fetched_at_idx
    ON cache_entries(fetched_at);
