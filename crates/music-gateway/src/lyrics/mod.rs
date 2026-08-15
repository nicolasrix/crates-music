//! Per-track lyrics: resolution, external provider, and HTTP surface.
//!
//! The storage half lives in `music-recommend` (`LyricsStore`, migration
//! `0023_track_lyrics.sql`) because the table sits in that crate's pool
//! next to `track_metadata`, which supplies the external lookup key.
//! Everything *policy* — which source to trust, in what order, with what
//! timeouts and TTLs — lives here.
//!
//! Why the gateway resolves rather than each client: one household shares
//! one cache, the PWA needs the answer stored locally to work offline,
//! LRC is parsed exactly once instead of in both the web client and the
//! TUI, and there is a single egress point with one User-Agent and one
//! switch (`[lyrics] external_lookup`) to stop talking to a third party.

pub mod handlers;
pub mod lrc;
pub mod lrclib;
pub mod resolver;

pub use lrclib::{LrclibClient, LrclibError, LrclibHit};
pub use resolver::{LyricsResolver, ResolveError};
