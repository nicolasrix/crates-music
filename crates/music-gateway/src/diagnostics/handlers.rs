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
use music_recommend::types::{EmbeddingKey, ModelVersion};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::diagnostics::{
    ChildAgg, ChildBreakdown, ClientEventRecord, HistogramBucket, RecommendSummary, SpanPoint,
    SpanRecord,
};
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

// --- /v1/diagnostics/span_series ------------------------------------------

/// Hard ceiling on points per response. A span fired 10/s for an hour
/// is 36k samples — already chunky for one fetch. Caller-supplied
/// `limit` is clamped to this so a runaway `?limit=999999` can't OOM.
const MAX_SPAN_SERIES_POINTS: usize = 10_000;
const DEFAULT_SPAN_SERIES_POINTS: usize = 2_000;

#[derive(Debug, Deserialize)]
pub struct SpanSeriesQuery {
    /// Required: the span name to plot. We deliberately don't default
    /// to "all", since the response is a flat list of points without
    /// per-name tagging — the histogram endpoint covers the all-names
    /// case.
    name: String,
    since_ms: Option<i64>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct SpanSeriesResponse {
    name: String,
    points: Vec<SpanSeriesPoint>,
}

#[derive(Debug, Serialize)]
pub struct SpanSeriesPoint {
    end_ms: i64,
    duration_ms: i64,
}

impl From<SpanPoint> for SpanSeriesPoint {
    fn from(p: SpanPoint) -> Self {
        Self {
            end_ms: p.end_ms,
            duration_ms: p.duration_ms,
        }
    }
}

pub async fn span_series(
    State(state): State<AppState>,
    Query(q): Query<SpanSeriesQuery>,
) -> Result<Json<SpanSeriesResponse>, (StatusCode, Json<Value>)> {
    let limit = q
        .limit
        .unwrap_or(DEFAULT_SPAN_SERIES_POINTS)
        .clamp(1, MAX_SPAN_SERIES_POINTS);
    let pts = state
        .trace_store()
        .span_series(&q.name, q.since_ms, limit)
        .await
        .map_err(db_error)?;
    Ok(Json(SpanSeriesResponse {
        name: q.name,
        points: pts.into_iter().map(SpanSeriesPoint::from).collect(),
    }))
}

// --- /v1/diagnostics/span_children ----------------------------------------

#[derive(Debug, Deserialize)]
pub struct SpanChildrenQuery {
    name: String,
    since_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct SpanChildrenResponse {
    parent_name: String,
    parent_count: u64,
    parent_sum_ms: i64,
    children: Vec<ChildAggJson>,
}

#[derive(Debug, Serialize)]
pub struct ChildAggJson {
    name: String,
    count: u64,
    sum_ms: i64,
    mean_ms: f64,
}

impl From<ChildAgg> for ChildAggJson {
    fn from(c: ChildAgg) -> Self {
        #[allow(clippy::cast_precision_loss)] // u64 count fits in f64 at our cardinality
        let mean_ms = if c.count == 0 {
            0.0
        } else {
            c.sum_ms as f64 / c.count as f64
        };
        Self {
            name: c.name,
            count: c.count,
            sum_ms: c.sum_ms,
            mean_ms,
        }
    }
}

impl From<ChildBreakdown> for SpanChildrenResponse {
    fn from(b: ChildBreakdown) -> Self {
        Self {
            parent_name: b.parent_name,
            parent_count: b.parent_count,
            parent_sum_ms: b.parent_sum_ms,
            children: b.children.into_iter().map(ChildAggJson::from).collect(),
        }
    }
}

pub async fn span_children(
    State(state): State<AppState>,
    Query(q): Query<SpanChildrenQuery>,
) -> Result<Json<SpanChildrenResponse>, (StatusCode, Json<Value>)> {
    let breakdown = state
        .trace_store()
        .child_breakdown(&q.name, q.since_ms)
        .await
        .map_err(db_error)?;
    Ok(Json(SpanChildrenResponse::from(breakdown)))
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

/// Per-field length caps on client-supplied RUM fields. The emitter
/// produces short, structured values; anything larger is a bug or a
/// client trying to bloat the (un-trimmed) ring. Reject the batch with
/// 422 rather than truncate, so the telemetry isn't silently corrupted.
const MAX_SESSION_ID_LEN: usize = 64;
const MAX_NAME_LEN: usize = 128;
const MAX_PAGE_PATH_LEN: usize = 512;
const MAX_RATING_LEN: usize = 32;
/// Serialized-`fields` JSON budget per event.
const MAX_FIELDS_BYTES: usize = 4096;

/// Ring capacity for the `client_events` table, mirroring the `spans`
/// ring (`TRACES_MAX_ROWS`). Trimmed after each insert so the table
/// can't grow without bound on a long-lived gateway.
const CLIENT_EVENTS_MAX_ROWS: usize = 100_000;

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
            let too_long = if e.session_id.len() > MAX_SESSION_ID_LEN {
                Some("session_id")
            } else if e.name.len() > MAX_NAME_LEN {
                Some("name")
            } else if e.page_path.len() > MAX_PAGE_PATH_LEN {
                Some("page_path")
            } else if e.rating.as_ref().is_some_and(|r| r.len() > MAX_RATING_LEN) {
                Some("rating")
            } else if fields_json.len() > MAX_FIELDS_BYTES {
                Some("fields")
            } else {
                None
            };
            if let Some(field) = too_long {
                return Err(field);
            }
            Ok(ClientEventRecord {
                received_ms,
                occurred_ms: e.occurred_ms,
                session_id: e.session_id,
                name: e.name,
                value_ms: e.value_ms,
                rating: e.rating,
                page_path: e.page_path,
                user_agent: user_agent.clone(),
                fields_json,
            })
        })
        .collect::<Result<Vec<_>, &'static str>>()
        .map_err(|field| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "event field exceeds size limit",
                    "field": field,
                })),
            )
        })?;
    let n = records.len();
    let store = state.trace_store();
    store
        .insert_client_events(records)
        .await
        .map_err(db_error)?;
    // Bound the ring right after the write (no drainer for RUM).
    store
        .trim_client_events_to_capacity(CLIENT_EVENTS_MAX_ROWS)
        .await
        .map_err(db_error)?;
    Ok(Json(ClientEventsAccepted { accepted: n }))
}

pub async fn list_client_events(
    State(state): State<AppState>,
    Query(q): Query<ClientEventsQuery>,
) -> Result<Json<ClientEventsResponse>, (StatusCode, Json<Value>)> {
    let limit = q
        .limit
        .unwrap_or(DEFAULT_TRACE_LIMIT)
        .clamp(1, MAX_TRACE_LIMIT);
    let rows = state
        .trace_store()
        .recent_client_events(limit, q.name.as_deref())
        .await
        .map_err(db_error)?;
    Ok(Json(ClientEventsResponse {
        events: rows.into_iter().map(ClientEventEntry::from).collect(),
    }))
}

// --- /v1/diagnostics/recently_played ---------------------------------------

const DEFAULT_RECENTLY_PLAYED_LIMIT: u32 = 100;
const MAX_RECENTLY_PLAYED_LIMIT: u32 = 1_000;

#[derive(Debug, Deserialize)]
pub struct RecentlyPlayedQuery {
    limit: Option<u32>,
    /// Inclusive lower bound on `occurred_at` (unix-ms). Useful when
    /// eyeballing recency-window candidates: pass `now - window_ms` and
    /// see how many distinct tracks the user replayed inside that
    /// window.
    since_ms: Option<i64>,
}

/// One scrobble row, joined against the gateway's `track_metadata`
/// cache so the diagnostics panel can show "title — artist — album · year"
/// instead of opaque track ids. Metadata fields are `Option` because the
/// cache may not yet contain a row for tracks that haven't been embedded
/// (recommend ingest is what populates `track_metadata`); the UI falls
/// back to the raw `track_id` in that case.
#[derive(Debug, Serialize)]
pub struct RecentlyPlayedEntry {
    track_id: String,
    /// Client-supplied scrobble timestamp.
    occurred_at_ms: i64,
    /// Gateway-stamped persist time. Difference from `occurred_at_ms`
    /// = client clock skew + offline batch delay.
    received_at_ms: i64,
    title: Option<String>,
    artist: Option<String>,
    artist_id: Option<String>,
    album: Option<String>,
    album_id: Option<String>,
    year: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct RecentlyPlayedResponse {
    events: Vec<RecentlyPlayedEntry>,
}

pub async fn recently_played(
    State(state): State<AppState>,
    Query(q): Query<RecentlyPlayedQuery>,
) -> Result<Json<RecentlyPlayedResponse>, (StatusCode, Json<Value>)> {
    let limit = q
        .limit
        .unwrap_or(DEFAULT_RECENTLY_PLAYED_LIMIT)
        .clamp(1, MAX_RECENTLY_PLAYED_LIMIT);
    let rows = state
        .event_store()
        .recently_played(limit, q.since_ms)
        .await
        .map_err(db_error)?;

    // Batched metadata join: one query for the whole page, not N. The
    // deduplication via .collect() into a HashSet→Vec keeps the round
    // trip linear in *unique* tracks, not events — matters when a user
    // has played the same track several times in the window.
    let unique_ids: Vec<music_core::TrackId> = rows
        .iter()
        .map(|e| e.track_id.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let metadata = state
        .metadata_store()
        .get_many(&unique_ids)
        .await
        .map_err(db_error)?;

    let events = rows
        .into_iter()
        .map(|e| {
            let md = metadata.get(&e.track_id);
            RecentlyPlayedEntry {
                track_id: e.track_id.as_str().to_string(),
                occurred_at_ms: e.occurred_at,
                received_at_ms: e.received_at,
                title: md.map(|m| m.title.clone()),
                artist: md.map(|m| m.artist.clone()),
                artist_id: md.and_then(|m| m.artist_id.clone()),
                album: md.and_then(|m| m.album.clone()),
                album_id: md.and_then(|m| m.album_id.clone()),
                year: md.and_then(|m| m.year),
            }
        })
        .collect();
    Ok(Json(RecentlyPlayedResponse { events }))
}

// --- /v1/diagnostics/recommend/* ------------------------------------------
//
// Four aggregations over recommend spans. They share an upstream
// (`TraceStore::recommend_summaries`) and a since_ms filter; each
// handler turns the raw rows into one chart's worth of JSON.

#[derive(Debug, Deserialize)]
pub struct RecommendQuery {
    /// Inclusive lower bound on the span's `end_ms`. `None` ⇒ scan the
    /// whole ring (still bounded by `trim_to_capacity`).
    since_ms: Option<i64>,
}

/// Bucket boundaries for the queue-fill histogram. Six bins for the
/// shortfall range + a dedicated 100% bin so the "everything is fine"
/// case stays separable from "we delivered 19/20 occasionally."
const FILL_BUCKETS: &[(&str, f32, f32)] = &[
    ("0%", 0.0, 0.0001),
    ("0-20%", 0.0001, 0.20),
    ("20-40%", 0.20, 0.40),
    ("40-60%", 0.40, 0.60),
    ("60-80%", 0.60, 0.80),
    ("80-100%", 0.80, 0.999_999),
    ("100%", 0.999_999, f32::INFINITY),
];

#[derive(Debug, Serialize)]
pub struct FillBucket {
    label: &'static str,
    count: u64,
}

#[derive(Debug, Serialize)]
pub struct QueueFillResponse {
    total: u64,
    buckets: Vec<FillBucket>,
}

pub async fn recommend_queue_fill(
    State(state): State<AppState>,
    Query(q): Query<RecommendQuery>,
) -> Result<Json<QueueFillResponse>, (StatusCode, Json<Value>)> {
    let rows = state
        .trace_store()
        .recommend_summaries(q.since_ms)
        .await
        .map_err(db_error)?;

    let mut buckets: Vec<FillBucket> = FILL_BUCKETS
        .iter()
        .map(|(label, ..)| FillBucket { label, count: 0 })
        .collect();
    let mut total: u64 = 0;
    for row in &rows {
        // Rows without both fields contribute to neither numerator nor
        // denominator — they're old-shape and uninterpretable here.
        let (Some(req), Some(res)) = (row.requested_n, row.results) else {
            continue;
        };
        if req == 0 {
            continue;
        }
        total += 1;
        // Clamp at 1.0 — "over-delivered" lands in the 100% bin alongside
        // exact-fill, which is the operator's intuition.
        let ratio = (f32::from(u16::try_from(res.min(req)).unwrap_or(u16::MAX))
            / f32::from(u16::try_from(req).unwrap_or(u16::MAX)))
        .clamp(0.0, 1.0);
        for (i, (_label, lo, hi)) in FILL_BUCKETS.iter().enumerate() {
            if ratio >= *lo && ratio < *hi {
                buckets[i].count += 1;
                break;
            }
        }
    }
    Ok(Json(QueueFillResponse { total, buckets }))
}

#[derive(Debug, Serialize)]
pub struct ShortfallResponse {
    total: u64,
    /// Map of `shortfall_reason` → count. Rows with no reason recorded
    /// are bucketed under `unknown` so the operator can see "how much of
    /// the ring is pre-R1?" at a glance.
    counts: std::collections::BTreeMap<String, u64>,
}

pub async fn recommend_shortfall(
    State(state): State<AppState>,
    Query(q): Query<RecommendQuery>,
) -> Result<Json<ShortfallResponse>, (StatusCode, Json<Value>)> {
    let rows = state
        .trace_store()
        .recommend_summaries(q.since_ms)
        .await
        .map_err(db_error)?;
    let mut counts: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for row in &rows {
        let key = row
            .shortfall_reason
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        *counts.entry(key).or_insert(0) += 1;
    }
    let total = u64::try_from(rows.len()).unwrap_or(u64::MAX);
    Ok(Json(ShortfallResponse { total, counts }))
}

#[derive(Debug, Serialize)]
pub struct SimilarityResponse {
    count: u64,
    /// Quantiles over the *combined* admitted-similarity sample across
    /// the window. Useful for spotting drift ("everything we recommend
    /// has crept above 0.95 — we're stuck in a corner").
    p50: f64,
    p90: f64,
    p95: f64,
    p99: f64,
    min: f64,
    max: f64,
    mean: f64,
}

pub async fn recommend_similarity(
    State(state): State<AppState>,
    Query(q): Query<RecommendQuery>,
) -> Result<Json<SimilarityResponse>, (StatusCode, Json<Value>)> {
    let rows = state
        .trace_store()
        .recommend_summaries(q.since_ms)
        .await
        .map_err(db_error)?;
    // Flatten everyone's admitted_sims into one population. We could
    // emit per-row stats, but the chart this feeds plots one quantile
    // per refresh — the combined sample matches the visual.
    let mut sims: Vec<f32> = Vec::new();
    for row in &rows {
        sims.extend(row.admitted_sims.iter().copied());
    }
    if sims.is_empty() {
        return Ok(Json(SimilarityResponse {
            count: 0,
            p50: 0.0,
            p90: 0.0,
            p95: 0.0,
            p99: 0.0,
            min: 0.0,
            max: 0.0,
            mean: 0.0,
        }));
    }
    sims.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sims.len();
    let pick = |p: usize| -> f64 {
        // Nearest-rank — matches `TraceStore::histogram` for consistency.
        let idx = (p.saturating_mul(n).div_ceil(100))
            .saturating_sub(1)
            .min(n - 1);
        f64::from(sims[idx])
    };
    #[allow(clippy::cast_precision_loss)] // n bounded by ring cap × top_n
    let mean: f64 = f64::from(sims.iter().sum::<f32>()) / n as f64;
    Ok(Json(SimilarityResponse {
        count: u64::try_from(n).unwrap_or(u64::MAX),
        p50: pick(50),
        p90: pick(90),
        p95: pick(95),
        p99: pick(99),
        min: f64::from(*sims.first().expect("non-empty")),
        max: f64::from(*sims.last().expect("non-empty")),
        mean,
    }))
}

const DEFAULT_TOP_RESULTS_LIMIT: usize = 20;
const MAX_TOP_RESULTS_LIMIT: usize = 200;

#[derive(Debug, Deserialize)]
pub struct TopResultsQuery {
    since_ms: Option<i64>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct TopResultItem {
    track_id: String,
    count: u64,
    /// Title from the gateway metadata cache if present, otherwise
    /// `None`. The UI falls back to displaying the raw `track_id`.
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TopResultsResponse {
    items: Vec<TopResultItem>,
}

pub async fn recommend_top_results(
    State(state): State<AppState>,
    Query(q): Query<TopResultsQuery>,
) -> Result<Json<TopResultsResponse>, (StatusCode, Json<Value>)> {
    let limit = q
        .limit
        .unwrap_or(DEFAULT_TOP_RESULTS_LIMIT)
        .clamp(1, MAX_TOP_RESULTS_LIMIT);
    let rows = state
        .trace_store()
        .recommend_summaries(q.since_ms)
        .await
        .map_err(db_error)?;
    let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for row in &rows {
        for id in &row.result_track_ids {
            *counts.entry(id.clone()).or_insert(0) += 1;
        }
    }
    // Sort by count desc, ties broken by track_id asc for determinism.
    let mut ranked: Vec<(String, u64)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(limit);

    // Single metadata round-trip for the page.
    let track_ids: Vec<music_core::TrackId> = ranked
        .iter()
        .map(|(id, _)| music_core::TrackId::from(id.as_str()))
        .collect();
    let metadata = state
        .metadata_store()
        .get_many(&track_ids)
        .await
        .unwrap_or_default();

    let items = ranked
        .into_iter()
        .map(|(track_id, count)| {
            let tid = music_core::TrackId::from(track_id.as_str());
            let md = metadata.get(&tid);
            TopResultItem {
                track_id,
                count,
                title: md.map(|m| m.title.clone()),
                artist: md.map(|m| m.artist.clone()),
                album: md.and_then(|m| m.album.clone()),
            }
        })
        .collect();
    Ok(Json(TopResultsResponse { items }))
}

/// Suppress the dead-code warning — `RecommendSummary` is re-exported
/// from `diagnostics::mod` and is the input type for the four handlers
/// above; some test builds don't see the public re-export as a use.
#[allow(dead_code)]
fn _ensure_summary_in_scope(_: RecommendSummary) {}

// --- /v1/diagnostics/recommend/feedback ----------------------------------

const DEFAULT_FEEDBACK_LIMIT: i64 = 50;
const MAX_FEEDBACK_LIMIT: i64 = 500;

#[derive(Debug, Deserialize)]
pub struct FeedbackQuery {
    since_ms: Option<i64>,
    limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct FeedbackItem {
    track_id: String,
    up: i64,
    down: i64,
    last_voted_ms: i64,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FeedbackListResponse {
    items: Vec<FeedbackItem>,
}

/// Per-track up/down aggregates over `since_ms`, joined with the
/// metadata cache. Sorted newest-voted first by `FeedbackStore::aggregate`.
pub async fn recommend_feedback(
    State(state): State<AppState>,
    Query(q): Query<FeedbackQuery>,
) -> Result<Json<FeedbackListResponse>, (StatusCode, Json<Value>)> {
    let limit = q
        .limit
        .unwrap_or(DEFAULT_FEEDBACK_LIMIT)
        .clamp(1, MAX_FEEDBACK_LIMIT);
    let rows = state
        .feedback()
        .aggregate(q.since_ms, limit)
        .await
        .map_err(db_error)?;

    let ids: Vec<music_core::TrackId> = rows
        .iter()
        .map(|r| music_core::TrackId::from(r.track_id.as_str()))
        .collect();
    let metadata = state
        .metadata_store()
        .get_many(&ids)
        .await
        .unwrap_or_default();

    let items = rows
        .into_iter()
        .map(|r| {
            let tid = music_core::TrackId::from(r.track_id.as_str());
            let md = metadata.get(&tid);
            FeedbackItem {
                track_id: r.track_id,
                up: r.up,
                down: r.down,
                last_voted_ms: r.last_voted_ms,
                title: md.map(|m| m.title.clone()),
                artist: md.map(|m| m.artist.clone()),
                album: md.and_then(|m| m.album.clone()),
            }
        })
        .collect();
    Ok(Json(FeedbackListResponse { items }))
}

// --- /v1/diagnostics/recommend/latent_space -------------------------------

/// Suffix marking a 3-D UMAP projection. Matches the Python reducer's
/// convention in `default_proj_version(n_components=3)`.
const D3_PROJ_SUFFIX: &str = "-d3";

#[derive(Debug, Deserialize)]
pub struct LatentSpaceQuery {
    /// Explicit projection version to render. When omitted, the handler
    /// picks the projection with the most recent `created_at_ms` for
    /// the resolved `model_version` — i.e. "whatever the reducer wrote
    /// last."
    proj_version: Option<String>,
    /// Embedding model to scope the lookup to. Defaults to the
    /// gateway's active `recommend_model_version` (matches what the
    /// rest of the recommender pipeline writes).
    model_version: Option<String>,
    /// Layout preference: `"2d"` snaps to the newest non-`-d3`
    /// projection, `"3d"` to the newest `-d3` projection. Ignored when
    /// `proj_version` is explicitly set (debug pathway). The web UI
    /// uses this to drive the canvas off the colour-by selector
    /// without needing a separate `proj_version` dropdown.
    prefer: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LatentSpacePoint {
    track_id: String,
    x: f64,
    y: f64,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    /// Reported genre from Subsonic's `child` tag, cached in
    /// `track_metadata`. The web scatter colours clusters by this — a
    /// rough validation of "does the audio embedding recover the
    /// human-labelled grouping?"
    genre: Option<String>,
    /// First four PCA components on the original embedding space,
    /// computed by the reducer alongside `(x, y)` (migration 0009).
    /// `null` per-component on projections that predate the migration
    /// or for components past the dataset's natural rank. Powers the
    /// web "colour by → PCn" mode.
    pc1: Option<f64>,
    pc2: Option<f64>,
    pc3: Option<f64>,
    pc4: Option<f64>,
    /// Third UMAP axis from an `n_components=3` reducer run (migration
    /// 0010). `null` for 2D projections. Powers the web "colour by →
    /// UMAP z" mode — the only continuous channel whose interpretation
    /// is "another UMAP-discovered axis" rather than a PCA component.
    z: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct LatentSpaceVersionEntry {
    proj_version: String,
    point_count: i64,
    created_at_ms: i64,
}

#[derive(Debug, Serialize)]
pub struct LatentSpaceResponse {
    /// Active model these points belong to. Echoed back so the client
    /// can label the chart without tracking the request shape.
    model_version: String,
    /// The projection actually rendered. `None` when no projection has
    /// been written yet for this model.
    proj_version: Option<String>,
    points: Vec<LatentSpacePoint>,
    /// Catalogue of all known projections for `model_version`, newest
    /// first. Populates the proj-version dropdown without a second
    /// round-trip — the payload is small (one row per UMAP run).
    versions: Vec<LatentSpaceVersionEntry>,
}

/// 2-D UMAP scatter for the latent-space diagnostics page.
///
/// Two-step resolution:
///   1. Figure out which `proj_version` to render — explicit param wins,
///      otherwise pick the newest from `proj_versions_for_model`. The
///      2-D and 3-D UMAP runs are independent layouts; the dropdown
///      surfaces both and the user picks one.
///   2. Fetch the points, then bulk-join the metadata cache so the UI
///      can show titles + artists without a per-row round-trip.
///
/// Each row is self-contained: `(x, y)` plus PCs and (for `-d3` runs)
/// `z` all come from the same UMAP run, so a 2-D-vs-3-D toggle is just
/// a `proj_version` switch — no cross-projection joining.
pub async fn recommend_latent_space(
    State(state): State<AppState>,
    Query(q): Query<LatentSpaceQuery>,
) -> Result<Json<LatentSpaceResponse>, (StatusCode, Json<Value>)> {
    let model_version = q.model_version.map_or_else(
        || state.recommend_model_version().clone(),
        ModelVersion::from,
    );

    let versions = state
        .projection()
        .proj_versions_for_model(&model_version)
        .await
        .map_err(db_error)?;

    // Resolve the active proj_version. Precedence:
    //   1. Explicit `proj_version` query param (debug pathway) — even
    //      if no row matches, return its empty result rather than
    //      silently swapping in a different projection.
    //   2. `prefer=2d|3d` — newest projection matching the suffix
    //      convention. `None` (and an empty point list) when no
    //      matching projection exists.
    //   3. Newest overall (legacy callers without either param).
    let proj_version = q.proj_version.or_else(|| match q.prefer.as_deref() {
        Some("2d") => versions
            .iter()
            .find(|v| !v.proj_version.ends_with(D3_PROJ_SUFFIX))
            .map(|v| v.proj_version.clone()),
        Some("3d") => versions
            .iter()
            .find(|v| v.proj_version.ends_with(D3_PROJ_SUFFIX))
            .map(|v| v.proj_version.clone()),
        _ => versions.first().map(|v| v.proj_version.clone()),
    });

    let points = if let Some(pv) = &proj_version {
        let raw = state
            .projection()
            .list_by_proj_version(pv, &model_version)
            .await
            .map_err(db_error)?;
        let ids: Vec<music_core::TrackId> = raw
            .iter()
            .map(|p| music_core::TrackId::from(p.track_id.as_str()))
            .collect();
        let metadata = state
            .metadata_store()
            .get_many(&ids)
            .await
            .unwrap_or_default();
        raw.into_iter()
            .map(|p| {
                let tid = music_core::TrackId::from(p.track_id.as_str());
                let md = metadata.get(&tid);
                LatentSpacePoint {
                    track_id: p.track_id,
                    x: p.x,
                    y: p.y,
                    title: md.map(|m| m.title.clone()),
                    artist: md.map(|m| m.artist.clone()),
                    album: md.and_then(|m| m.album.clone()),
                    genre: md.and_then(|m| m.genre.clone()),
                    pc1: p.pc1,
                    pc2: p.pc2,
                    pc3: p.pc3,
                    pc4: p.pc4,
                    z: p.z,
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    let version_entries = versions
        .into_iter()
        .map(|v| LatentSpaceVersionEntry {
            proj_version: v.proj_version,
            point_count: v.point_count,
            created_at_ms: v.created_at_ms,
        })
        .collect();

    Ok(Json(LatentSpaceResponse {
        model_version: model_version.as_str().to_string(),
        proj_version,
        points,
        versions: version_entries,
    }))
}

// --- /v1/diagnostics/recommend/latent_neighbours --------------------------
//
// Hover overlay for the latent-space scatter. Returns the k nearest
// neighbours of a seed track in the **original** CLAP space — the
// distances UMAP doesn't preserve. Cheap (HNSW query is sub-ms at our
// scale) and fetched on demand from the web client, so the
// /latent_space payload stays small.

const DEFAULT_LATENT_NEIGHBOURS_K: usize = 10;
const MAX_LATENT_NEIGHBOURS_K: usize = 100;

#[derive(Debug, Deserialize)]
pub struct LatentNeighboursQuery {
    track_id: String,
    /// Number of neighbours to return. Defaults to
    /// `DEFAULT_LATENT_NEIGHBOURS_K`. Clamped to
    /// `MAX_LATENT_NEIGHBOURS_K` to keep the response bounded.
    k: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct LatentNeighbourEntry {
    track_id: String,
    /// Cosine distance in CLAP space: `1 - cosine_similarity`. Range
    /// `[0, 2]`, with `0` = identical direction and `1` = orthogonal.
    /// Reported (instead of similarity) because the visual encoding
    /// reads as distance: short line = close, long line = far.
    cosine_distance: f64,
}

#[derive(Debug, Serialize)]
pub struct LatentNeighboursResponse {
    /// Echoed back so the client can correlate the response to its
    /// (possibly stale) hover state.
    track_id: String,
    /// Ordered by ascending `cosine_distance`. Seed itself is filtered
    /// out, so the list contains at most `k` entries.
    neighbours: Vec<LatentNeighbourEntry>,
}

/// k nearest neighbours of `track_id` in the full embedding space.
/// Returns 404 if the seed has no vector in the ANN — the caller is
/// hovering a point that exists in the projection (otherwise they
/// couldn't hover it) but the embedding can in principle be missing
/// after a model-version flip; the UI degrades to "no overlay" for
/// that point.
pub async fn recommend_latent_neighbours(
    State(state): State<AppState>,
    Query(q): Query<LatentNeighboursQuery>,
) -> Result<Json<LatentNeighboursResponse>, (StatusCode, &'static str)> {
    let k_raw = q.k.unwrap_or(DEFAULT_LATENT_NEIGHBOURS_K);
    if k_raw == 0 {
        return Err((StatusCode::BAD_REQUEST, "k must be >= 1"));
    }
    let k = k_raw.min(MAX_LATENT_NEIGHBOURS_K);
    let seed_id = music_core::TrackId::from(q.track_id.as_str());
    let ann = state.ann();
    let vector = match ann.get_vector(&seed_id) {
        Ok(Some(v)) => v,
        Ok(None) => return Err((StatusCode::NOT_FOUND, "seed not embedded")),
        Err(_) => return Err((StatusCode::INTERNAL_SERVER_ERROR, "ann query failed")),
    };
    let results = ann
        .query_excluding(&vector, k, std::slice::from_ref(&seed_id))
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "ann query failed"))?;
    let neighbours = results
        .into_iter()
        .map(|r| LatentNeighbourEntry {
            track_id: r.track_id.into_inner(),
            cosine_distance: (1.0 - f64::from(r.similarity)).max(0.0),
        })
        .collect();
    Ok(Json(LatentNeighboursResponse {
        track_id: q.track_id,
        neighbours,
    }))
}

pub async fn queue_depth(
    State(state): State<AppState>,
    Query(q): Query<QueueDepthQuery>,
) -> Result<Json<QueueDepthResponse>, (StatusCode, Json<Value>)> {
    let model_version = q.model_version.map_or_else(
        || state.recommend_model_version().clone(),
        ModelVersion::from,
    );
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

// --- /v1/diagnostics/recommend/sessions ----------------------------------

const DEFAULT_SESSIONS_LIMIT: i64 = 50;
const MAX_SESSIONS_LIMIT: i64 = 500;
/// Cap on events fetched per session when `include_events=1`. Sessions
/// don't realistically grow this large (they auto-close on the next
/// StartSession), but a hard cap protects the endpoint from a
/// pathological queue.
const MAX_EVENTS_PER_SESSION: u32 = 500;

#[derive(Debug, Deserialize)]
pub struct SessionsQuery {
    limit: Option<i64>,
    /// `1` to attach `events` + `segments` to each item. Defaults to off
    /// because the join is O(events × embedding-fetches) per session
    /// and the unaugmented response is what list views actually need.
    include_events: Option<u8>,
    /// Model version used when looking up embeddings for the per-
    /// segment cosine distances. Defaults to the gateway's active
    /// `recommend_model_version`. Ignored when `include_events != 1`.
    model_version: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SessionEventItem {
    track_id: String,
    /// Lowercase enum string: "scrobble" | "skip" | "seek" | …
    /// Matches the wire format the event log already exposes elsewhere.
    event_type: String,
    occurred_at_ms: i64,
}

#[derive(Debug, Serialize)]
pub struct SessionSegment {
    /// Cosine distance in `[0, 2]` between event[i].track and
    /// event[i+1].track under the active model_version. `None` when
    /// either track has no `done` embedding row (a session can hop
    /// through not-yet-ingested tracks).
    cosine_distance: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct SessionItem {
    session_id: String,
    anchor_track_id: String,
    items_count: i64,
    started_ms: i64,
    /// `None` when the session is still active.
    ended_ms: Option<i64>,
    /// How many events (scrobble/skip/seek/…) landed under this session
    /// id. 0 when nothing has been logged yet — useful indicator of
    /// "user opened a queue but didn't actually listen."
    event_count: i64,
    /// Present only when `include_events=1`. Oldest-first.
    #[serde(skip_serializing_if = "Option::is_none")]
    events: Option<Vec<SessionEventItem>>,
    /// Present only when `include_events=1`. Length = events.len() - 1;
    /// `segments[i]` is the gap between `events[i]` and `events[i+1]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    segments: Option<Vec<SessionSegment>>,
}

#[derive(Debug, Serialize)]
pub struct SessionsListResponse {
    items: Vec<SessionItem>,
}

/// Cosine distance between two equal-length vectors, in `[0, 2]`.
/// Returns `None` for length mismatch / empty input / zero-norm — all
/// "no meaningful distance" cases that should surface as JSON `null`,
/// not 500.
fn cosine_distance(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.is_empty() || a.len() != b.len() {
        return None;
    }
    let mut dot = 0.0_f64;
    let mut norm_a = 0.0_f64;
    let mut norm_b = 0.0_f64;
    for (ai, bi) in a.iter().zip(b.iter()) {
        let x = f64::from(*ai);
        let y = f64::from(*bi);
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return None;
    }
    let sim = (dot / (norm_a.sqrt() * norm_b.sqrt())).clamp(-1.0, 1.0);
    Some(1.0 - sim)
}

/// Recent recommend-session lifetimes, newest started_ms first. Each
/// row joins in the count of events stamped with that session_id from
/// the (separately stored) event log. Single SQL aggregate, not N+1.
pub async fn recommend_sessions(
    State(state): State<AppState>,
    Query(q): Query<SessionsQuery>,
) -> Result<Json<SessionsListResponse>, (StatusCode, Json<Value>)> {
    let limit = q
        .limit
        .unwrap_or(DEFAULT_SESSIONS_LIMIT)
        .clamp(1, MAX_SESSIONS_LIMIT);
    let include_events = q.include_events.unwrap_or(0) == 1;
    let rows = state.sessions().recent(limit).await.map_err(db_error)?;
    let session_ids: Vec<music_core::SessionId> =
        rows.iter().map(|r| r.session_id.clone()).collect();
    let counts = state
        .event_store()
        .count_events_per_session(&session_ids)
        .await
        .map_err(db_error)?;

    let model_version = if include_events {
        Some(q.model_version.map_or_else(
            || state.recommend_model_version().clone(),
            ModelVersion::from,
        ))
    } else {
        None
    };

    let mut items = Vec::with_capacity(rows.len());
    for r in rows {
        let event_count = counts.get(&r.session_id).copied().unwrap_or(0);
        let (events, segments) = if let Some(model) = model_version.as_ref() {
            let session_events = state
                .event_store()
                .by_session(&r.session_id, MAX_EVENTS_PER_SESSION)
                .await
                .map_err(db_error)?;
            let segs = compute_session_segments(&state, model, &session_events)
                .await
                .map_err(db_error)?;
            let evs: Vec<SessionEventItem> = session_events
                .into_iter()
                .map(|e| SessionEventItem {
                    track_id: e.track_id.as_str().to_string(),
                    event_type: e.event_type.as_str().to_string(),
                    occurred_at_ms: e.occurred_at,
                })
                .collect();
            (Some(evs), Some(segs))
        } else {
            (None, None)
        };
        items.push(SessionItem {
            session_id: r.session_id.as_str().to_string(),
            anchor_track_id: r.anchor_track_id.as_str().to_string(),
            items_count: r.items_count,
            started_ms: r.started_ms,
            ended_ms: r.ended_ms,
            event_count,
            events,
            segments,
        });
    }
    Ok(Json(SessionsListResponse { items }))
}

/// Compute per-segment cosine distances for an ordered event list. We
/// dedupe by track_id before fetching embeddings so a session that
/// loops the same track only does one lookup per unique id.
async fn compute_session_segments(
    state: &AppState,
    model_version: &ModelVersion,
    events: &[music_recommend::StoredEvent],
) -> Result<Vec<SessionSegment>, music_recommend::Error> {
    use std::collections::HashMap;
    if events.len() < 2 {
        return Ok(Vec::new());
    }
    let mut vectors: HashMap<music_core::TrackId, Option<Vec<f32>>> = HashMap::new();
    for e in events {
        if !vectors.contains_key(&e.track_id) {
            let key = EmbeddingKey::new(e.track_id.clone(), model_version.clone());
            let vec = state
                .embedding_store()
                .get(&key)
                .await?
                .map(|emb| emb.vector);
            vectors.insert(e.track_id.clone(), vec);
        }
    }
    let mut out = Vec::with_capacity(events.len() - 1);
    for win in events.windows(2) {
        let a = vectors.get(&win[0].track_id).and_then(|v| v.as_deref());
        let b = vectors.get(&win[1].track_id).and_then(|v| v.as_deref());
        let cosine_distance = match (a, b) {
            (Some(a), Some(b)) => cosine_distance(a, b),
            _ => None,
        };
        out.push(SessionSegment { cosine_distance });
    }
    Ok(out)
}
