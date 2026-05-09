//! HTTP handlers for `/v1/diagnostics/*`.
//!
//! Three endpoints:
//! - `GET /v1/diagnostics/traces` — recent closed spans, optionally
//!   filtered by name / since.
//! - `GET /v1/diagnostics/histogram` — per-name duration histogram.
//! - `GET /v1/diagnostics/queue_depth` — embedding ingest queue
//!   counts (NotStarted / InProgress / Done / Failed).
//!
//! Mounted under the protected sub-router so the existing
//! `require_bearer` layer enforces auth — diagnostics expose internal
//! timing detail, so they're authenticated like everything else.
//!
//! All three return JSON. Time-bounds are unix-millisecond integers
//! (`since_ms`) to match what `SpanRecord` already stores; no Duration
//! parsing on the wire.

use axum::{Json, extract::Query, extract::State, http::StatusCode};
use music_recommend::types::ModelVersion;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::diagnostics::{HistogramBucket, SpanRecord};
use crate::state::AppState;

const DEFAULT_TRACE_LIMIT: usize = 100;
const MAX_TRACE_LIMIT: usize = 1_000;

/// Internal helper: convert any sqlx error to a 500 with a generic body.
/// We don't echo the SQL error to the client — it's auth-walled but
/// could still leak schema hints.
fn db_error<E: std::fmt::Display>(err: E) -> (StatusCode, Json<Value>) {
    tracing::error!(error = %err, "diagnostics DB error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({"error": "diagnostics database error"})),
    )
}

// --- /v1/diagnostics/traces ------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TracesQuery {
    /// Cap rows returned. Bounded server-side to `MAX_TRACE_LIMIT` so
    /// a hostile / accidental `?limit=999999` can't OOM the response.
    #[serde(default)]
    limit: Option<usize>,
    name: Option<String>,
    since_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct TracesResponse {
    traces: Vec<TraceEntry>,
}

/// A `SpanRecord` with conveniences for the UI: derived `duration_ms`,
/// and `fields_json` parsed back to a JSON object so clients don't
/// have to do a second parse. On parse failure we surface the raw
/// string under `_raw` rather than failing the request — bench/test
/// fixtures sometimes emit non-JSON, and a single bad row shouldn't
/// blow up the whole listing.
#[derive(Debug, Serialize)]
pub struct TraceEntry {
    trace_id: String,
    span_id: i64,
    parent_span_id: Option<i64>,
    name: String,
    target: String,
    start_ms: i64,
    end_ms: i64,
    duration_ms: i64,
    fields: Value,
}

impl From<SpanRecord> for TraceEntry {
    fn from(s: SpanRecord) -> Self {
        let duration_ms = s.duration_ms();
        let fields = serde_json::from_str::<Value>(&s.fields_json)
            .unwrap_or_else(|_| serde_json::json!({"_raw": s.fields_json}));
        Self {
            trace_id: s.trace_id,
            span_id: s.span_id,
            parent_span_id: s.parent_span_id,
            name: s.name,
            target: s.target,
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            duration_ms,
            fields,
        }
    }
}

pub async fn traces(
    State(state): State<AppState>,
    Query(q): Query<TracesQuery>,
) -> Result<Json<TracesResponse>, (StatusCode, Json<Value>)> {
    let limit = q
        .limit
        .unwrap_or(DEFAULT_TRACE_LIMIT)
        .clamp(1, MAX_TRACE_LIMIT);
    let rows = state
        .trace_store()
        .query(limit, q.name.as_deref(), q.since_ms)
        .await
        .map_err(db_error)?;
    Ok(Json(TracesResponse {
        traces: rows.into_iter().map(TraceEntry::from).collect(),
    }))
}

// --- /v1/diagnostics/histogram ---------------------------------------------

#[derive(Debug, Deserialize)]
pub struct HistogramQuery {
    since_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct HistogramResponse {
    buckets: Vec<HistogramBucketJson>,
}

#[derive(Debug, Serialize)]
pub struct HistogramBucketJson {
    name: String,
    count: usize,
    min_ms: i64,
    max_ms: i64,
    p50_ms: i64,
    p95_ms: i64,
    p99_ms: i64,
    mean_ms: f64,
}

impl From<HistogramBucket> for HistogramBucketJson {
    fn from(b: HistogramBucket) -> Self {
        Self {
            name: b.name,
            count: b.count,
            min_ms: b.min_ms,
            max_ms: b.max_ms,
            p50_ms: b.p50_ms,
            p95_ms: b.p95_ms,
            p99_ms: b.p99_ms,
            mean_ms: b.mean_ms,
        }
    }
}

pub async fn histogram(
    State(state): State<AppState>,
    Query(q): Query<HistogramQuery>,
) -> Result<Json<HistogramResponse>, (StatusCode, Json<Value>)> {
    let buckets = state
        .trace_store()
        .histogram(q.since_ms)
        .await
        .map_err(db_error)?;
    Ok(Json(HistogramResponse {
        buckets: buckets.into_iter().map(HistogramBucketJson::from).collect(),
    }))
}

// --- /v1/diagnostics/queue_depth ------------------------------------------

#[derive(Debug, Deserialize)]
pub struct QueueDepthQuery {
    /// Defaults to `AppState::recommend_model_version()`. An explicit
    /// override is useful when comparing migration progress between
    /// two model checkpoints during a rolling upgrade.
    model_version: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct QueueDepthResponse {
    model_version: String,
    not_started: u64,
    in_progress: u64,
    done: u64,
    failed: u64,
}

pub async fn queue_depth(
    State(state): State<AppState>,
    Query(q): Query<QueueDepthQuery>,
) -> Result<Json<QueueDepthResponse>, (StatusCode, Json<Value>)> {
    let model_version = q
        .model_version
        .map_or_else(|| state.recommend_model_version().clone(), ModelVersion::from);
    let counts = state
        .embedding_store()
        .counts(&model_version)
        .await
        .map_err(db_error)?;
    Ok(Json(QueueDepthResponse {
        model_version: model_version.as_str().to_string(),
        not_started: counts.not_started,
        in_progress: counts.in_progress,
        done: counts.done,
        failed: counts.failed,
    }))
}
