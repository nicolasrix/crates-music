-- Per-track audio embeddings + ingest status, content-addressed by
-- (track_id, model_version). A new model_version inserts new rows
-- alongside the old ones rather than invalidating them, so model
-- swaps are non-destructive and old vectors stay queryable.
--
-- `status` IS the ingest queue: rows with status = 'not_started' are
-- the work backlog, ordered by created_at. No separate queue table.
--
-- `vector` is a BLOB of dim * 4 little-endian f32 bytes. SQLite
-- handles 2 KB blobs comfortably; this is the source of truth from
-- which the ANN index can be rebuilt at any time.

CREATE TABLE IF NOT EXISTS track_embeddings (
    track_id        TEXT    NOT NULL,
    model_version   TEXT    NOT NULL,
    dim             INTEGER NOT NULL CHECK (dim >= 0),    -- 0 until embedded; > 0 once status = 'done'
    vector          BLOB,                                 -- nullable until status = 'done'
    status          TEXT    NOT NULL
                            CHECK (status IN ('not_started', 'in_progress', 'done', 'failed')),
    error           TEXT,                                 -- non-null only when status = 'failed'
    created_at      INTEGER NOT NULL,                     -- unix-ms, when first enqueued
    updated_at      INTEGER NOT NULL,                     -- unix-ms, last status transition
    PRIMARY KEY (track_id, model_version)
);

-- Worker uses this to pop the next 'not_started' row in FIFO order.
CREATE INDEX IF NOT EXISTS track_embeddings_queue_idx
    ON track_embeddings(status, created_at)
    WHERE status = 'not_started';

-- For "is this track embedded under any model?" lookups.
CREATE INDEX IF NOT EXISTS track_embeddings_track_idx
    ON track_embeddings(track_id);
