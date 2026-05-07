//! Domain types for `crates-music`.
//!
//! Pure data with no I/O. Other crates (`music-subsonic`, `music-cache`,
//! ...) depend on these types and convert their wire formats into them.

pub mod album;
pub mod artist;
pub mod ids;
pub mod track;

pub use album::Album;
pub use artist::Artist;
pub use ids::{AlbumId, ArtistId, TrackId};
pub use track::Track;
