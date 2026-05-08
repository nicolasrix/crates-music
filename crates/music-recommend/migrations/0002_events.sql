-- Append-only event log: scrobble, skip, like/unlike, seek. Feeds the
-- behavioural-similarity index in a later phase; for now it's pure
-- write-ahead persistence so we don't lose user-interaction signal
-- before the recommender consumes it.
--
-- Two timestamps on purpose:
--   * occurred_at — client-supplied unix ms (when the user hit play)
--   * received_at — gateway-side unix ms (when we persisted)
-- The gap between them = client clock skew + queue delay; useful for
-- diagnosis and lets us replay events in either order without
-- committing to a single source of truth on time.
CREATE TABLE events (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    event_type    TEXT    NOT NULL,
    track_id      TEXT    NOT NULL,
    occurred_at   INTEGER NOT NULL,
    received_at   INTEGER NOT NULL,
    metadata      TEXT
);

CREATE INDEX events_occurred_at_idx ON events (occurred_at);
CREATE INDEX events_track_id_idx    ON events (track_id);
CREATE INDEX events_event_type_idx  ON events (event_type);
