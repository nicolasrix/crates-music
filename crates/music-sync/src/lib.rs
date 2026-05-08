//! Cross-device playback sync.
//!
//! The gateway is the single linearizer for all sync ops. Clients submit
//! [`SyncOp`]s; the gateway applies them in arrival order to its
//! [`SyncState`] and broadcasts the new state version. Last-Writer-Wins
//! per field falls out of the linear order — no client-side vector clocks.
//!
//! This crate is pure logic: types, serde shapes, and the deterministic
//! state machine. Transport (HTTP snapshot + WebSocket fan-out) lives in
//! `music-gateway`; client-side optimistic UI lives in the apps.

mod ops;
mod state;

pub use ops::SyncOp;
pub use state::{ApplyError, SyncState};
