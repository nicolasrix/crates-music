-- Diagnostics span store. Ring-buffered timing data for the gateway's
-- ingest pipeline (and later, HTTP requests).
--
-- This DB lives in its own SQLite file because traces are throwaway
-- (oldest rows are evicted to bound disk usage) — distinct lifecycle
-- from OAuth state, where data loss is intolerable. Each trace row is
-- one closed `tracing` span; consumers reconstruct trees client-side.

CREATE TABLE IF NOT EXISTS spans (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    trace_id        TEXT NOT NULL,                  -- shared by all spans of one logical operation
    span_id         INTEGER NOT NULL,               -- tracing::Id::into_u64
    parent_span_id  INTEGER,                        -- NULL = root
    name            TEXT NOT NULL,                  -- e.g. "ingest.process_next"
    target          TEXT NOT NULL,                  -- module path
    start_ms        INTEGER NOT NULL,               -- unix-ms; span opened
    end_ms          INTEGER NOT NULL,               -- unix-ms; span closed
    fields_json     TEXT NOT NULL DEFAULT '{}'      -- attribute map as JSON
);

-- Recent-traces query reads the largest `id`s.
CREATE INDEX IF NOT EXISTS spans_id_desc_idx ON spans(id DESC);

-- Histogram queries filter by name and time range.
CREATE INDEX IF NOT EXISTS spans_name_end_ms_idx ON spans(name, end_ms);

-- Trace-tree reconstruction: fetch all spans for a trace_id.
CREATE INDEX IF NOT EXISTS spans_trace_id_idx ON spans(trace_id);
