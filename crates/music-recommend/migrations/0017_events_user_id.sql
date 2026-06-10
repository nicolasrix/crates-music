-- Per-user partition of the append-only event log (multi-user, PR E).
--
-- Taste signal (scrobble/skip/like/seek) must attribute to the user who
-- generated it so that one household member's behaviour never feeds
-- another's recommender, and guest activity can be dropped from training
-- entirely (enforced gateway-side, not here).
--
-- `user_id` is a plain indexed integer, NOT a foreign key: this is the
-- recommend pool (`gateway-state.recommend.sqlite`), a separate SQLite
-- file from the `users` table (`gateway-state.sqlite`), so cross-file
-- FKs aren't available. The id mirrors `users.id`; resolution lives in
-- the gateway's `Principal`.
--
-- DEFAULT 1 backfills every existing row to the owner (users.id=1), so
-- pre-migration signal stays attributed and live across the upgrade.
-- `id` stays the AUTOINCREMENT primary key — user_id is a filter column,
-- not part of identity here — so a plain ADD COLUMN suffices.
ALTER TABLE events ADD COLUMN user_id INTEGER NOT NULL DEFAULT 1;

CREATE INDEX events_user_id_idx ON events (user_id);
