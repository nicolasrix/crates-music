-- Per-user partition of the recommendation provenance log (multi-user,
-- PR E).
--
-- A served recommendation belongs to the room it was served into — i.e.
-- the host user whose taste profile generated it (guests read the host's
-- recs read-only). Stamping user_id keeps the training substrate
-- attributable per user. `recommendation_item` partitions transitively
-- through its FK to `recommendation`, so only the parent needs the
-- column. `id` stays the AUTOINCREMENT primary key, so ADD COLUMN
-- suffices.
--
-- DEFAULT 1 backfills existing rows to the owner. user_id is a plain
-- indexed integer mirroring `users.id` (no cross-file FK; see 0017).
ALTER TABLE recommendation ADD COLUMN user_id INTEGER NOT NULL DEFAULT 1;

CREATE INDEX recommendation_user_id_idx ON recommendation (user_id);
