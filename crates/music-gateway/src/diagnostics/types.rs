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

/// One browser RUM event uploaded by a web client. Two timestamps —
/// `occurred_ms` from the client and `received_ms` stamped by the
/// gateway — are kept distinct because client clocks drift, and the
/// diagnostics feed must order on a clock the operator controls.
///
/// `value_ms` is `f64` because Web Vitals are typically fractional
/// (e.g. LCP=1234.5 ms); it is `None` for non-timing marks like
/// `playback.user_skipped`. `rating` is the `web-vitals` library's
/// bucket (`good` / `needs-improvement` / `poor`) — `None` for custom
/// marks. `user_agent` is captured from the request header and
/// truncated server-side; we never trust the client to bound it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ClientEventRecord {
    pub received_ms: i64,
    pub occurred_ms: i64,
    pub session_id: String,
    pub name: String,
    pub value_ms: Option<f64>,
    pub rating: Option<String>,
    pub page_path: String,
    pub user_agent: Option<String>,
    pub fields_json: String,
}
