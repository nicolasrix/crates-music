-- Guest codes: a host User (or admin) mints a shareable code; redeeming
-- it creates an ephemeral guest principal attached to that host's room.
-- (PR D of the user-system plan — docs/plans/user-system.md §4.3.)
--
-- The plaintext code is never stored — only sha256(code), like every
-- other secret in this DB. `host_user_id` drives both the guest's sync
-- room and their (read-only) recommendation context. A code can be
-- bounded by `expires_at` and/or `max_uses`; NULL on either means
-- "unbounded on that axis". `revoked_at` is a soft kill-switch.
--
-- An explicit AUTOINCREMENT id gives the host UI a stable, guessable-free
-- handle for DELETE without exposing the code hash.

CREATE TABLE guest_codes (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    code_hash    TEXT NOT NULL UNIQUE,            -- sha256(code)
    host_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    label        TEXT,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER,                          -- NULL = until revoked
    max_uses     INTEGER,                          -- NULL = unlimited
    uses         INTEGER NOT NULL DEFAULT 0,
    revoked_at   INTEGER
);

CREATE INDEX guest_codes_host_idx ON guest_codes(host_user_id);
