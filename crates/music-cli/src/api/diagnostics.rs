//! Read-only fetchers for the gateway's `/v1/diagnostics/*` endpoints, powering
//! the TUI's admin-only Diagnostics section (parity plan Phase 9). Every call
//! is a plain bearer-authed GET; the gateway 403s non-admins (the section is
//! hidden for them, but the server is the real gate — decision D3), surfaced as
//! [`ApiError::Forbidden`]. Field names mirror the gateway's serde structs in
//! `music-gateway/src/diagnostics/handlers.rs` verbatim (no `rename`s there).

use anyhow::{Context, anyhow};
use serde::Deserialize;

use super::ApiError;
use crate::config::Config;
use crate::gateway::{endpoint, http_client, require_gateway};

// ── ingest ────────────────────────────────────────────────────────────────

/// `queue_depth`: embedding-ingest queue counts for one model version.
#[derive(Debug, Clone, Deserialize)]
pub struct QueueDepth {
    pub model_version: String,
    pub not_started: u64,
    pub in_progress: u64,
    pub done: u64,
    pub failed: u64,
}

// ── tracing ───────────────────────────────────────────────────────────────

/// One closed span from `traces`. `fields` is free-form JSON (the gateway
/// stores every span's structured fields; a parse failure arrives as
/// `{"_raw": "…"}`).
#[derive(Debug, Clone, Deserialize)]
pub struct TraceEntry {
    pub trace_id: String,
    pub span_id: i64,
    pub parent_span_id: Option<i64>,
    pub name: String,
    pub target: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub duration_ms: i64,
    #[serde(default)]
    pub fields: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
struct TracesResponse {
    traces: Vec<TraceEntry>,
}

/// One per-span-name duration bucket from `histogram` (quantiles are computed
/// server-side via nearest-rank).
#[derive(Debug, Clone, Deserialize)]
pub struct HistogramBucket {
    pub name: String,
    pub count: usize,
    pub min_ms: i64,
    pub max_ms: i64,
    pub p50_ms: i64,
    pub p95_ms: i64,
    pub p99_ms: i64,
    pub mean_ms: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct HistogramResponse {
    buckets: Vec<HistogramBucket>,
}

// ── client events (RUM) ────────────────────────────────────────────────────

/// One browser RUM event from `client_events`. Two timestamps: `occurred_ms`
/// (client clock) and `received_ms` (gateway-stamped).
#[derive(Debug, Clone, Deserialize)]
pub struct ClientEvent {
    pub received_ms: i64,
    pub occurred_ms: i64,
    pub session_id: String,
    pub name: String,
    pub value_ms: Option<f64>,
    pub rating: Option<String>,
    pub page_path: String,
    pub user_agent: Option<String>,
    #[serde(default)]
    pub fields: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
struct ClientEventsResponse {
    events: Vec<ClientEvent>,
}

// ── listening (recently played) ────────────────────────────────────────────

/// One `recently_played` row: a play event with its (best-effort) hydrated
/// track metadata.
#[derive(Debug, Clone, Deserialize)]
pub struct RecentlyPlayed {
    pub track_id: String,
    pub occurred_at_ms: i64,
    pub received_at_ms: i64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub artist_id: Option<String>,
    pub album: Option<String>,
    pub album_id: Option<String>,
    pub year: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
struct RecentlyPlayedResponse {
    events: Vec<RecentlyPlayed>,
}

// ── recommender inspectors ─────────────────────────────────────────────────

/// One bar of the `recommend/queue_fill` histogram (fixed fill-ratio labels).
#[derive(Debug, Clone, Deserialize)]
pub struct FillBucket {
    pub label: String,
    pub count: u64,
}

/// `recommend/queue_fill`: how full autoplay refills came back, bucketed.
#[derive(Debug, Clone, Deserialize)]
pub struct QueueFill {
    pub total: u64,
    pub buckets: Vec<FillBucket>,
}

/// `recommend/shortfall`: why refills under-delivered (reason → count).
#[derive(Debug, Clone, Deserialize)]
pub struct Shortfall {
    pub total: u64,
    pub counts: std::collections::BTreeMap<String, u64>,
}

/// `recommend/similarity`: distribution of served-candidate cosine scores.
#[derive(Debug, Clone, Deserialize)]
pub struct Similarity {
    pub count: u64,
    pub p50: f64,
    pub p90: f64,
    pub p95: f64,
    pub p99: f64,
    pub min: f64,
    pub max: f64,
    pub mean: f64,
}

/// One `recommend/top_results` leaderboard row: a track and how often it was
/// served as a recommendation in the window.
#[derive(Debug, Clone, Deserialize)]
pub struct TopResultItem {
    pub track_id: String,
    pub count: u64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TopResultsResponse {
    items: Vec<TopResultItem>,
}

/// The Recommender sub-tab's four panels, fetched together for one window.
#[derive(Debug, Clone)]
pub struct RecommenderPanels {
    pub queue_fill: QueueFill,
    pub shortfall: Shortfall,
    pub similarity: Similarity,
    pub top_results: Vec<TopResultItem>,
}

// ── latent space ───────────────────────────────────────────────────────────

/// One projected point from `recommend/latent_space` (2-D; the `pc*`/`z`
/// fields the gateway also returns are unused here).
#[derive(Debug, Clone, Deserialize)]
pub struct LatentPoint {
    pub track_id: String,
    pub x: f64,
    pub y: f64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
}

/// `recommend/latent_space`: the whole 2-D scatter for one projection version.
/// `proj_version` is `None` when the gateway has no projection yet (an empty
/// scatter, not an error).
#[derive(Debug, Clone, Deserialize)]
pub struct LatentSpace {
    pub model_version: String,
    pub proj_version: Option<String>,
    pub points: Vec<LatentPoint>,
}

/// One `recommend/latent_neighbours` entry: a track near the seed in the raw
/// embedding space (cosine distance, smaller = closer).
#[derive(Debug, Clone, Deserialize)]
pub struct LatentNeighbour {
    pub track_id: String,
    pub cosine_distance: f64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct LatentNeighboursResponse {
    neighbours: Vec<LatentNeighbour>,
}

// ── HTTP plumbing ──────────────────────────────────────────────────────────

/// A `since_ms` query fragment: one pair, or empty for "all time".
fn since_query(since_ms: Option<i64>) -> Vec<(&'static str, String)> {
    since_ms.map_or_else(Vec::new, |ms| vec![("since_ms", ms.to_string())])
}

/// Shared bearer-authed GET + JSON parse, with 403 mapped to
/// [`ApiError::Forbidden`] (non-admin) so the caller renders an honest notice.
async fn diag_get<T: serde::de::DeserializeOwned>(
    config: &Config,
    path: &str,
    query: &[(&str, String)],
) -> Result<T, ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, path);
    let resp = http_client(gw)?
        .get(&url)
        .query(query)
        .bearer_auth(&token)
        .send()
        .await
        .context("requesting diagnostics")?;
    if resp.status().as_u16() == 403 {
        return Err(ApiError::Forbidden);
    }
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        // A latent seed with no stored vector (or no projection) 404s — not a
        // hard error; surface it the same as a warming recommender so the UI
        // shows a friendly notice.
        return Err(ApiError::RecommenderUnavailable);
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("diagnostics request failed ({status}): {text}").into());
    }
    resp.json()
        .await
        .context("parsing diagnostics response")
        .map_err(Into::into)
}

/// `GET /v1/diagnostics/queue_depth` — ingest queue counts (default model).
pub async fn queue_depth(config: &Config) -> Result<QueueDepth, ApiError> {
    diag_get(config, "/v1/diagnostics/queue_depth", &[]).await
}

/// `GET /v1/diagnostics/traces?limit=` — recent closed spans, newest first.
pub async fn traces(config: &Config, limit: usize) -> Result<Vec<TraceEntry>, ApiError> {
    let r: TracesResponse = diag_get(
        config,
        "/v1/diagnostics/traces",
        &[("limit", limit.to_string())],
    )
    .await?;
    Ok(r.traces)
}

/// `GET /v1/diagnostics/histogram?since_ms=` — per-span-name duration buckets.
pub async fn histogram(
    config: &Config,
    since_ms: Option<i64>,
) -> Result<Vec<HistogramBucket>, ApiError> {
    let r: HistogramResponse =
        diag_get(config, "/v1/diagnostics/histogram", &since_query(since_ms)).await?;
    Ok(r.buckets)
}

/// `GET /v1/diagnostics/client_events?limit=` — recent browser RUM events.
pub async fn client_events(config: &Config, limit: usize) -> Result<Vec<ClientEvent>, ApiError> {
    let r: ClientEventsResponse = diag_get(
        config,
        "/v1/diagnostics/client_events",
        &[("limit", limit.to_string())],
    )
    .await?;
    Ok(r.events)
}

/// `GET /v1/diagnostics/recently_played?limit=` — recent play events.
pub async fn recently_played(
    config: &Config,
    limit: u32,
) -> Result<Vec<RecentlyPlayed>, ApiError> {
    let r: RecentlyPlayedResponse = diag_get(
        config,
        "/v1/diagnostics/recently_played",
        &[("limit", limit.to_string())],
    )
    .await?;
    Ok(r.events)
}

async fn queue_fill(config: &Config, since_ms: Option<i64>) -> Result<QueueFill, ApiError> {
    diag_get(
        config,
        "/v1/diagnostics/recommend/queue_fill",
        &since_query(since_ms),
    )
    .await
}

async fn shortfall(config: &Config, since_ms: Option<i64>) -> Result<Shortfall, ApiError> {
    diag_get(
        config,
        "/v1/diagnostics/recommend/shortfall",
        &since_query(since_ms),
    )
    .await
}

async fn similarity(config: &Config, since_ms: Option<i64>) -> Result<Similarity, ApiError> {
    diag_get(
        config,
        "/v1/diagnostics/recommend/similarity",
        &since_query(since_ms),
    )
    .await
}

async fn top_results(
    config: &Config,
    since_ms: Option<i64>,
    limit: usize,
) -> Result<Vec<TopResultItem>, ApiError> {
    let mut query = since_query(since_ms);
    query.push(("limit", limit.to_string()));
    let r: TopResultsResponse =
        diag_get(config, "/v1/diagnostics/recommend/top_results", &query).await?;
    Ok(r.items)
}

/// The Recommender sub-tab: all four inspectors for one window, fetched
/// concurrently. A single failure fails the whole panel (they share a window
/// and target the same gateway — a failure is systemic, not per-panel).
pub async fn recommender_panels(
    config: &Config,
    since_ms: Option<i64>,
) -> Result<RecommenderPanels, ApiError> {
    let (queue_fill, shortfall, similarity, top_results) = tokio::try_join!(
        queue_fill(config, since_ms),
        shortfall(config, since_ms),
        similarity(config, since_ms),
        top_results(config, since_ms, TOP_RESULTS_LIMIT),
    )?;
    Ok(RecommenderPanels {
        queue_fill,
        shortfall,
        similarity,
        top_results,
    })
}

/// `GET /v1/diagnostics/recommend/latent_space` — the 2-D scatter (newest
/// projection). Empty `points` when no projection exists yet.
pub async fn latent_space(config: &Config) -> Result<LatentSpace, ApiError> {
    diag_get(
        config,
        "/v1/diagnostics/recommend/latent_space",
        &[("prefer", "2d".to_owned())],
    )
    .await
}

/// `GET /v1/diagnostics/recommend/latent_neighbours?track_id=&k=` — the raw
/// nearest neighbours of a seed track. 404 (seed not embedded) →
/// [`ApiError::RecommenderUnavailable`].
pub async fn latent_neighbours(
    config: &Config,
    track_id: &str,
    k: usize,
) -> Result<Vec<LatentNeighbour>, ApiError> {
    let r: LatentNeighboursResponse = diag_get(
        config,
        "/v1/diagnostics/recommend/latent_neighbours",
        &[
            ("track_id", track_id.to_owned()),
            ("k", k.to_string()),
        ],
    )
    .await?;
    Ok(r.neighbours)
}

/// Rows requested for the top-results leaderboard.
const TOP_RESULTS_LIMIT: usize = 20;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_depth_parses() {
        let q: QueueDepth = serde_json::from_str(
            r#"{"model_version":"clamp3","not_started":3,"in_progress":1,"done":7349,"failed":31}"#,
        )
        .unwrap();
        assert_eq!(q.done, 7349);
        assert_eq!(q.failed, 31);
    }

    #[test]
    fn shortfall_counts_is_a_map() {
        let s: Shortfall = serde_json::from_str(
            r#"{"total":5,"counts":{"unindexed_seed":3,"unknown":2}}"#,
        )
        .unwrap();
        assert_eq!(s.total, 5);
        assert_eq!(s.counts.get("unindexed_seed"), Some(&3));
    }

    #[test]
    fn latent_space_tolerates_no_projection() {
        // proj_version null + empty points = "no projection yet", not an error.
        let l: LatentSpace =
            serde_json::from_str(r#"{"model_version":"clamp3","proj_version":null,"points":[]}"#)
                .unwrap();
        assert!(l.proj_version.is_none());
        assert!(l.points.is_empty());
    }

    #[test]
    fn latent_point_ignores_extra_pc_fields() {
        let l: LatentSpace = serde_json::from_str(
            r#"{"model_version":"m","proj_version":"m-2d","points":[
                {"track_id":"t1","x":0.1,"y":-0.2,"title":"A","artist":"B","album":"C",
                 "genre":"Jazz","pc1":1.0,"pc2":2.0,"pc3":null,"pc4":null,"z":null}]}"#,
        )
        .unwrap();
        assert_eq!(l.points.len(), 1);
        assert_eq!(l.points[0].genre.as_deref(), Some("Jazz"));
    }

    #[test]
    fn since_query_omits_when_all_time() {
        assert!(since_query(None).is_empty());
        assert_eq!(since_query(Some(42)), vec![("since_ms", "42".to_owned())]);
    }
}
