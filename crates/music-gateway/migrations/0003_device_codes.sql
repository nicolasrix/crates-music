-- Device Authorization Grant (RFC 8628) state.
--
-- The CLI (a public client that can't host a redirect URI) asks for a
-- device_code + user_code, prints the short user_code, and polls the token
-- endpoint. A logged-in browser approves or denies the user_code. Only the
-- sha256 of the high-entropy device_code is stored; the user_code is short
-- and low-entropy by design (the user types it) so it's kept in plaintext
-- for lookup — short TTL + the session-gated approval page are its defense.
--
-- Conventions match 0001/0002: *_hash PK, unix-ms INTEGER timestamps,
-- client_id FK, nullable decision/consume timestamps.
CREATE TABLE IF NOT EXISTS device_codes (
    device_code_hash TEXT PRIMARY KEY NOT NULL,                 -- sha256(device_code) hex
    user_code        TEXT NOT NULL UNIQUE,                      -- shown to the user, e.g. BDWP-HQML
    client_id        TEXT NOT NULL REFERENCES oauth_clients(client_id),
    issued_at        INTEGER NOT NULL,
    expires_at       INTEGER NOT NULL,                          -- unix-ms; ~10 min TTL
    interval_secs    INTEGER NOT NULL,                          -- polling interval handed to the client
    approved_at      INTEGER,                                   -- non-NULL = user approved
    denied_at        INTEGER,                                   -- non-NULL = user denied
    consumed_at      INTEGER                                    -- non-NULL = tokens already issued
);

CREATE INDEX IF NOT EXISTS device_codes_expires_idx   ON device_codes(expires_at);
CREATE INDEX IF NOT EXISTS device_codes_user_code_idx ON device_codes(user_code);
