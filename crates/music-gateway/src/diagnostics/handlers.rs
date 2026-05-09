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

use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
};
use music_recommend::types::ModelVersion;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::diagnostics::{ClientEventRecord, HistogramBucket, SpanRecord};
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

// --- /v1/diagnostics/client_events ----------------------------------------

/// Cap on a single batch. The web emitter flushes every ~10 s or on
/// `pagehide`; in steady state a session emits at most a handful of
/// vitals + a small number of custom marks per minute, so 50 is
/// generous. Rejecting at the edge avoids quadratic INSERT work and
/// caps the worst-case row burst per request.
const MAX_BATCH: usize = 50;

/// Truncation budget for `user_agent`. Any real browser UA fits in
/// ~200 bytes; 256 leaves headroom while preventing a hostile client
/// from filling the table with multi-MB strings.
const MAX_USER_AGENT_LEN: usize = 256;

#[derive(Debug, Deserialize)]
pub struct ClientEventInput {
    /// Random per-page-load identifier. Lets the diagnostics page
    /// group events from one session even when the path changes.
    session_id: String,
    /// Client-side wall clock (unix-ms) at the moment the event fired.
    occurred_ms: i64,
    /// Event name, e.g. `web-vital.LCP` or `playback.start`.
    name: String,
    /// Web-vital value or custom-mark duration. Optional because some
    /// marks are events without a duration (e.g. `playback.user_skipped`).
    value_ms: Option<f64>,
    /// `web-vitals` library bucket (`good`/`needs-improvement`/`poor`)
    /// or `None` for custom marks.
    rating: Option<String>,
    /// Path the user was viewing when the event fired.
    page_path: String,
    /// Free-form attributes; serialized as `fields_json` in storage.
    #[serde(default)]
    fields: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct ClientEventsRequest {
    events: Vec<ClientEventInput>,
}

#[derive(Debug, Serialize)]
pub struct ClientEventsAccepted {
    accepted: usize,
}

#[derive(Debug, Serialize)]
pub struct ClientEventEntry {
    received_ms: i64,
    occurred_ms: i64,
    session_id: String,
    name: String,
    value_ms: Option<f64>,
    rating: Option<String>,
    page_path: String,
    user_agent: Option<String>,
    fields: Value,
}

impl From<ClientEventRecord> for ClientEventEntry {
    fn from(r: ClientEventRecord) -> Self {
        let fields = serde_json::from_str::<Value>(&r.fields_json)
            .unwrap_or_else(|_| serde_json::json!({"_raw": r.fields_json}));
        Self {
            received_ms: r.received_ms,
            occurred_ms: r.occurred_ms,
            session_id: r.session_id,
            name: r.name,
            value_ms: r.value_ms,
            rating: r.rating,
            page_path: r.page_path,
            user_agent: r.user_agent,
            fields,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ClientEventsResponse {
    events: Vec<ClientEventEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ClientEventsQuery {
    limit: Option<usize>,
    name: Option<String>,
}

fn now_unix_ms() -> i64 {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(dur.as_millis()).unwrap_or(i64::MAX)
}

fn user_agent_from(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::USER_AGENT)?.to_str().ok()?;
    // Truncate at char boundary — `take(N)` on chars, not bytes — so we
    // never split a multi-byte codepoint and corrupt UTF-8.
    let truncated: String = raw.chars().take(MAX_USER_AGENT_LEN).collect();
    Some(truncated)
}

pub async fn submit_client_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ClientEventsRequest>,
) -> Result<Json<ClientEventsAccepted>, (StatusCode, Json<Value>)> {
    if body.events.len() > MAX_BATCH {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({
                "error": "batch too large",
                "max_batch": MAX_BATCH,
            })),
        ));
    }
    if body.events.is_empty() {
        return Ok(Json(ClientEventsAccepted { accepted: 0 }));
    }

    let received_ms = now_unix_ms();
    let user_agent = user_agent_from(&headers);

    let records: Vec<ClientEventRecord> = body
        .events
        .into_iter()
        .map(|e| {
            let fields_json = e
                .fields
                .as_ref()
                .map_or_else(|| "{}".to_string(), ToString::to_string);
            ClientEventRecord {
                received_ms,
                occurred_ms: e.occurred_ms,
                session_id: e.session_id,
                name: e.name,
                value_ms: e.value_ms,
                rating: e.rating,
                page_path: e.page_path,
                user_agent: user_agent.clone(),
                fields_json,
            }
        })
        .collect();
    let n = records.len();
    state
        .trace_store()
        .insert_client_events(records)
        .await
        .map_err(db_error)?;
    Ok(Json(ClientEventsAccepted { accepted: n }))
}

pub async fn list_client_events(
    State(state): State<AppState>,
    Query(q): Query<ClientEventsQuery>,
) -> Result<Json<ClientEventsResponse>, (StatusCode, Json<Value>)> {
    let limit = q.limit.unwrap_or(DEFAULT_TRACE_LIMIT).clamp(1, MAX_TRACE_LIMIT);
    let rows = state
        .trace_store()
        .recent_client_events(limit, q.name.as_deref())
        .await
        .map_err(db_error)?;
    Ok(Json(ClientEventsResponse {
        events: rows.into_iter().map(ClientEventEntry::from).collect(),
    }))
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
