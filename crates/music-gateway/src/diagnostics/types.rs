//! Shared types for the diagnostics module.

use serde::{Deserialize, Serialize};

/// One closed `tracing` span, ready to persist or render.
///
/// `span_id` and `parent_span_id` come straight from
/// `tracing::span::Id::into_u64()` (cast to i64 because SQLite
/// integers are signed). `trace_id` is computed by the layer as the
/// stringified id of the *root* span — a single trace tree shares one
/// `trace_id` and recovers its hierarchy via `parent_span_id`.
///
/// Fields recorded on the span are JSON-serialized into `fields_json`.
/// We don't break them out into typed columns because the schema has
/// to stay open: every instrumented function emits a different field
/// set, and the diagnostics page renders them as opaque key/value
/// pairs anyway.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpanRecord {
    pub trace_id: String,
    pub span_id: i64,
    pub parent_span_id: Option<i64>,
    pub name: String,
    pub target: String,
    /// Unix-ms when the span opened.
    pub start_ms: i64,
    /// Unix-ms when the span closed.
    pub end_ms: i64,
    /// JSON object encoding span attributes (`{"track_id":"t-7",...}`).
    pub fields_json: String,
}

impl SpanRecord {
    /// Convenience: closed-span duration in milliseconds.
    pub fn duration_ms(&self) -> i64 {
        self.end_ms - self.start_ms
    }
}
