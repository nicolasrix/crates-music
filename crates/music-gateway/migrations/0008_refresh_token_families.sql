-- Refresh-token reuse detection (OAuth 2.1 §4.3.1) — security review 1.5.
--
-- Rotation already revokes a refresh token on use (see
-- `consume_refresh_token`). What was missing: when an *already-rotated*
-- (revoked) token is replayed, that is the fingerprint of a stolen token
-- — the attacker and the legitimate client both hold a copy of the same
-- pre-rotation secret. The spec's response is to revoke the entire token
-- *family* (the whole rotation chain), logging out both parties.
--
-- To identify a family we tag every token with a `family_id` shared by
-- every token in its rotation chain. A brand-new grant (auth code /
-- device code) starts a fresh family (id = the token's own hash); each
-- rotation inherits its parent's family_id. Reuse of any revoked member
-- then revokes the whole family in one indexed UPDATE.
--
-- Backfill: pre-migration tokens each become their own family (their own
-- hash), so a token minted before this migration still gets a stable
-- family id the moment it is next rotated, and reuse detection covers it.

ALTER TABLE refresh_tokens ADD COLUMN family_id TEXT;

UPDATE refresh_tokens SET family_id = token_hash WHERE family_id IS NULL;

CREATE INDEX refresh_tokens_family_id_idx ON refresh_tokens(family_id);
