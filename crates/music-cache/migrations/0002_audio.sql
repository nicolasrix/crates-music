-- Audio cache: metadata in SQLite, blobs on disk.
--
-- `key` is the canonical (track_id, bitrate, codec) string. `blob_path` is
-- the disk file (relative to the cache root). LRU eviction orders by
-- `last_accessed_at` ASC where pinned = 0; pinned rows live in a separate
-- budget and are skipped by LRU.
CREATE TABLE IF NOT EXISTS audio_entries (
    key              TEXT    PRIMARY KEY NOT NULL,
    track_id         TEXT    NOT NULL,
    bitrate          INTEGER,
    codec            TEXT    NOT NULL,
    blob_path        TEXT    NOT NULL,
    bytes            INTEGER NOT NULL CHECK (bytes >= 0),
    last_accessed_at INTEGER NOT NULL,
    pinned           INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1))
);

CREATE INDEX IF NOT EXISTS audio_entries_track_id_idx
    ON audio_entries(track_id);

CREATE INDEX IF NOT EXISTS audio_entries_lru_idx
    ON audio_entries(pinned, last_accessed_at);
