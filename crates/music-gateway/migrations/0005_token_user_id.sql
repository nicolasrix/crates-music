-- Attach a `user_id` dimension to every token/session table so the
-- bearer middleware can resolve a Principal. (PR A of the user-system
-- plan — docs/plans/user-system.md.)
--
-- Nullable + backfilled to the owner (id=1): existing live tokens keep
-- working across the deploy with no re-login. New issuance defaults to 1
-- as well for now — PR B threads the real logged-in user_id through the
-- session -> auth_code -> token chain once multi-user login lands. The
-- bearer middleware treats a NULL user_id as the owner defensively, so a
-- missing value never denies a valid token.
--
-- ON DELETE CASCADE: deleting a user (e.g. reaping an expired guest in
-- PR D) drops their tokens and sessions with them.

ALTER TABLE access_tokens
    ADD COLUMN user_id INTEGER REFERENCES users(id) ON DELETE CASCADE;
ALTER TABLE refresh_tokens
    ADD COLUMN user_id INTEGER REFERENCES users(id) ON DELETE CASCADE;
ALTER TABLE auth_codes
    ADD COLUMN user_id INTEGER REFERENCES users(id) ON DELETE CASCADE;
ALTER TABLE device_codes
    ADD COLUMN user_id INTEGER REFERENCES users(id) ON DELETE CASCADE;
ALTER TABLE sessions
    ADD COLUMN user_id INTEGER REFERENCES users(id) ON DELETE CASCADE;

UPDATE access_tokens  SET user_id = 1 WHERE user_id IS NULL;
UPDATE refresh_tokens SET user_id = 1 WHERE user_id IS NULL;
UPDATE auth_codes     SET user_id = 1 WHERE user_id IS NULL;
UPDATE device_codes   SET user_id = 1 WHERE user_id IS NULL;
UPDATE sessions       SET user_id = 1 WHERE user_id IS NULL;

CREATE INDEX access_tokens_user_id_idx  ON access_tokens(user_id);
CREATE INDEX refresh_tokens_user_id_idx ON refresh_tokens(user_id);
