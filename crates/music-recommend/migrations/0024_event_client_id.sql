-- Client-chosen id per event, so a retried POST /v1/events can't land
-- the same event twice.
--
-- Why: the web client keeps events it could not deliver (a phone with the
-- screen off often has no network when a track ends) and retries them
-- later. When a request reached the gateway but its response was lost,
-- the retry carries events that are already here. A duplicated `skip`
-- would double its preference-affinity penalty, and a duplicated
-- `complete` would double-count a listen.
--
-- Nullable and scoped per user: REST callers and older clients send no
-- id, and their events keep persisting unconditionally, as before.
ALTER TABLE events ADD COLUMN client_event_id TEXT;

CREATE UNIQUE INDEX events_user_client_event_id_uq
    ON events (user_id, client_event_id)
    WHERE client_event_id IS NOT NULL;
