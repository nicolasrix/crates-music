-- Multi-user roles: relax the single-user lock and add identity/role
-- columns. (PR A of the user-system plan — docs/plans/user-system.md.)
--
-- The original `users` table hard-coded a single row via CHECK(id = 1).
-- SQLite can't drop a CHECK constraint in place, so we recreate the table
-- and copy the one existing row forward as the owner/admin. Every existing
-- deployment has exactly that one row (the master password), so the copy
-- is total.
--
-- Identity shape after this migration:
--   * Owner becomes id=1, role='admin', username='owner' (renameable later).
--   * `username`/`password_hash` are NULL for guests (PR D) — they have no
--     credentials, only an ephemeral expiring row.
--   * `host_user_id` links a guest to the User whose room they join (PR D);
--     ON DELETE CASCADE means deleting a host reaps their guests.
--   * `expires_at` bounds a guest session; NULL for real accounts.
--
-- No foreign keys reference `users` yet (the token/session tables gain
-- their `user_id` FK in 0005, after this), so dropping and recreating the
-- table here is safe — there are no dependents to break.

CREATE TABLE users_new (
    id            INTEGER PRIMARY KEY,            -- was CHECK(id = 1)
    username      TEXT UNIQUE,                    -- NULL for guests
    display_name  TEXT,
    role          TEXT NOT NULL DEFAULT 'admin'
                  CHECK (role IN ('admin', 'user', 'guest')),
    password_hash TEXT,                           -- NULL for guests
    host_user_id  INTEGER REFERENCES users_new(id) ON DELETE CASCADE, -- guests only
    expires_at    INTEGER,                        -- guests only (unix-ms)
    created_at    INTEGER NOT NULL
);

INSERT INTO users_new (id, username, display_name, role, password_hash, created_at)
    SELECT id, 'owner', 'Owner', 'admin', password_hash, created_at FROM users;

DROP TABLE users;
ALTER TABLE users_new RENAME TO users;

CREATE INDEX users_role_idx    ON users(role);
CREATE INDEX users_expires_idx ON users(expires_at);
