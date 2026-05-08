-- Browser sessions for the OAuth login flow.
--
-- Single-user gateway, so no `user_id` — a row simply means "the
-- operator is logged in via this browser." Cookie value is an opaque
-- 32-byte random token; we store sha256(token) only, never the token
-- itself.

CREATE TABLE IF NOT EXISTS sessions (
    token_hash      TEXT PRIMARY KEY NOT NULL,        -- sha256(cookie value) hex
    issued_at       INTEGER NOT NULL,                  -- unix-ms
    expires_at      INTEGER NOT NULL,                  -- unix-ms
    revoked_at      INTEGER                             -- unix-ms; non-NULL = revoked
);

CREATE INDEX IF NOT EXISTS sessions_expires_idx ON sessions(expires_at);
