//! Sync subsystem: server-authoritative playback state and the HTTP
//! endpoints that read/write it. WebSocket fan-out lives in `ws.rs`
//! (P5.3); this module owns the shared state and REST surface.

pub mod handlers;
pub mod store;
pub mod ws;

pub use store::SyncStore;
