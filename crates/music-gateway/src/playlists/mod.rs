//! Gateway-owned playlists (user-system PR F, decision D6).
//!
//! Playlist CRUD moves off Navidrome's `/rest/*` onto `/v1/playlists/*` so
//! membership can be private per-user. Navidrome stays catalog-only;
//! `store` holds Navidrome track ids, `handlers` enforces ownership +
//! visibility. See `docs/plans/user-system.md` §8.

pub mod handlers;
pub mod store;

pub use store::{PlaylistRow, PlaylistStore, TrackMode};
