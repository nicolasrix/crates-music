-- Browser RUM events. Each row is one Core Web Vital (LCP/INP/CLS/FCP/TTFB)
-- or one custom mark ("playback.start", "queue.reorder", ...) emitted by
-- a web client and uploaded as a batch.
--
-- Two timestamps because client and gateway clocks drift independently:
--   * occurred_ms — client's wall clock at the moment the event happened
--   * received_ms — gateway's wall clock when the batch landed; the
--     diagnostics page sorts on this so a phone with a wrong clock
--     can't poison the feed.
--
-- `value_ms` is REAL because web-vitals are typically fractional ms
-- (LCP can be e.g. 1234.567). `rating` is the bucket the web-vitals
-- library assigns ("good"|"needs-improvement"|"poor") — null for
-- custom marks, which don't have one.

CREATE TABLE IF NOT EXISTS client_events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    received_ms  INTEGER NOT NULL,
    occurred_ms  INTEGER NOT NULL,
    session_id   TEXT NOT NULL,
    name         TEXT NOT NULL,
    value_ms     REAL,
    rating       TEXT,
    page_path    TEXT NOT NULL,
    user_agent   TEXT,
    fields_json  TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS client_events_id_desc_idx ON client_events(id DESC);
CREATE INDEX IF NOT EXISTS client_events_name_received_idx ON client_events(name, received_ms);
CREATE INDEX IF NOT EXISTS client_events_session_idx ON client_events(session_id);
