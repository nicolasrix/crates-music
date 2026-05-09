//! Performance-monitoring layer for the gateway.
//!
//! Captures `tracing` spans into a SQLite ring buffer so we can
//! reconstruct timelines after the fact ("where did the last 30
//! ingest jobs spend their time?"). The store is throwaway — oldest
//! rows are evicted to bound disk usage — so it lives in its own
//! SQLite file rather than the OAuth state DB.
//!
//! Two surfaces:
//! - [`TraceStore`] — bulk-insert and query of [`SpanRecord`]s.
//! - [`TraceLayer`] (added in M0 step 2) — `tracing_subscriber::Layer`
//!   impl that feeds spans into the store via a background drainer.

mod layer;
mod store;
mod types;

pub use layer::{TraceLayer, spawn_drainer};
pub use store::TraceStore;
pub use types::SpanRecord;
