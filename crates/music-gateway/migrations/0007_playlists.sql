-- Gateway-owned playlists (user-system PR F, decision D6).
--
-- Playlists move OFF Navidrome's /rest/* so they can be private per-user.
-- Navidrome stays catalog-only; track ids here are Navidrome catalog ids
-- (the catalog is intentionally shared). One Navidrome account backs the
-- whole gateway, so per-user privacy can only live on our side.
--
-- owner_user_id references users(id) in this same file (gateway-state.sqlite),
-- so ON DELETE CASCADE cleans a removed user's playlists with them. Guests
-- never own playlists (enforced in the handler via the WritePlaylist
-- capability), so no guest rows ever land here.

CREATE TABLE playlists (
    id            TEXT PRIMARY KEY NOT NULL,       -- 128-bit random hex
    owner_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    visibility    TEXT NOT NULL DEFAULT 'private'  -- 'private' | 'shared'
                  CHECK (visibility IN ('private','shared')),
    created_ms    INTEGER NOT NULL,
    updated_ms    INTEGER NOT NULL
);

CREATE TABLE playlist_tracks (
    playlist_id   TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    position      INTEGER NOT NULL,
    track_id      TEXT NOT NULL,                   -- Navidrome catalog id
    PRIMARY KEY (playlist_id, position)
);

CREATE INDEX playlists_owner_idx ON playlists(owner_user_id);

-- One-time Navidrome → gateway playlist import is handled by
-- scripts/import_navidrome_playlists.py, which is idempotent by matching on
-- playlist name against the existing /v1/playlists set — no marker row
-- needed (and no admin endpoint to write one), so the schema stays to the
-- two tables the live CRUD path uses.
