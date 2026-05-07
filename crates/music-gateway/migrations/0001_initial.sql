-- Gateway state DB: OAuth 2.1 + sessions.
--
-- Lives in its own SQLite file (separate from the L2 cache) because the
-- two have different lifecycles: cache is throwaway, this DB holds
-- irreplaceable refresh tokens.
--
-- Single-user system: `users` is hardcoded to one row (id = 1).

CREATE TABLE IF NOT EXISTS users (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    password_hash   TEXT NOT NULL,           -- Argon2id PHC string
    created_at      INTEGER NOT NULL         -- unix-ms
);

CREATE TABLE IF NOT EXISTS oauth_clients (
    client_id       TEXT PRIMARY KEY NOT NULL,
    name            TEXT NOT NULL,           -- "web", "cli", phone label, …
    redirect_uris   TEXT NOT NULL,           -- JSON array of strings
    created_at      INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS auth_codes (
    code_hash               TEXT PRIMARY KEY NOT NULL,        -- sha256(code) hex
    client_id               TEXT NOT NULL REFERENCES oauth_clients(client_id),
    redirect_uri            TEXT NOT NULL,
    code_challenge          TEXT NOT NULL,
    code_challenge_method   TEXT NOT NULL CHECK (code_challenge_method = 'S256'),
    issued_at               INTEGER NOT NULL,                 -- unix-ms
    expires_at              INTEGER NOT NULL,                 -- unix-ms
    consumed_at             INTEGER                            -- unix-ms; non-NULL = exchanged
);

CREATE TABLE IF NOT EXISTS refresh_tokens (
    token_hash      TEXT PRIMARY KEY NOT NULL,                -- sha256(token) hex
    client_id       TEXT NOT NULL REFERENCES oauth_clients(client_id),
    device_label    TEXT,                                      -- shown on Devices page
    issued_at       INTEGER NOT NULL,
    expires_at      INTEGER,                                   -- NULL = no expiry; rotation-only revocation
    revoked_at      INTEGER
);

CREATE TABLE IF NOT EXISTS access_tokens (
    token_hash          TEXT PRIMARY KEY NOT NULL,
    client_id           TEXT NOT NULL REFERENCES oauth_clients(client_id),
    refresh_token_hash  TEXT REFERENCES refresh_tokens(token_hash) ON DELETE CASCADE,
    issued_at           INTEGER NOT NULL,
    expires_at          INTEGER NOT NULL,                     -- unix-ms; access tokens are short-lived
    revoked_at          INTEGER
);

CREATE INDEX IF NOT EXISTS access_tokens_expires_idx     ON access_tokens(expires_at);
CREATE INDEX IF NOT EXISTS refresh_tokens_client_id_idx  ON refresh_tokens(client_id);
CREATE INDEX IF NOT EXISTS auth_codes_expires_idx        ON auth_codes(expires_at);
