//! Typed gateway `/v1` fetchers, shared by the classic printing commands and
//! the interactive TUI.
//!
//! The split: this module does HTTP + parsing and returns data;
//! `ratings.rs` / `recommend.rs` stay as thin printing wrappers so the
//! classic commands' stdout is byte-identical, and the TUI consumes the same
//! typed results without scraping any of it. Auth goes through
//! [`crate::auth::resolve_bearer`] *per request*, so long-lived TUI sessions
//! pick up refreshed tokens naturally.

use anyhow::{Context, anyhow};
use futures_util::future::join_all;
use music_core::{Track, TrackId};
use music_subsonic::{Client, SearchResult3};
use reqwest::StatusCode;
use serde::Deserialize;

use crate::config::Config;
use crate::gateway::{endpoint, http_client, require_gateway};

/// Errors from the `/v1` fetchers, split so callers can render the one case
/// that isn't a hard failure — a degraded/warming recommender — as a
/// friendly notice instead of an error dump.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// `/v1/recommend/*` answered 404/503: no embedder, seed not embedded,
    /// or the index is still warming up. Not a bug, not a network problem.
    #[error("the gateway recommender is not ready")]
    RecommenderUnavailable,
    #[error(transparent)]
    Http(#[from] anyhow::Error),
}

/// One rated entity from `GET /v1/library/ratings`.
#[derive(Debug, Clone, Deserialize)]
pub struct RatingItem {
    pub kind: String,
    pub id: String,
    /// `"like"` / `"dislike"`; absent/`null` means neutral (the server
    /// doesn't return those, but be defensive).
    pub rating: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RatingsResponse {
    ratings: Vec<RatingItem>,
}

#[derive(Debug, Deserialize)]
struct RecommendItem {
    track_id: String,
}

#[derive(Debug, Deserialize)]
struct RecommendResponse {
    /// True when the recommender fell back to tag-only similarity (seed not
    /// embedded, or the index is still warming up). Stations don't send it.
    #[serde(default)]
    degraded: bool,
    results: Vec<RecommendItem>,
}

/// Ranked track ids from a recommend/station call. Rank order is the
/// similarity ranking — preserve it.
#[derive(Debug, Clone)]
pub struct RecommendList {
    pub degraded: bool,
    pub track_ids: Vec<TrackId>,
}

impl From<RecommendResponse> for RecommendList {
    fn from(resp: RecommendResponse) -> Self {
        Self {
            degraded: resp.degraded,
            track_ids: resp
                .results
                .into_iter()
                .map(|r| TrackId::from(r.track_id))
                .collect(),
        }
    }
}

/// `PUT /v1/library/rating` — set (`Some("like"|"dislike")`) or clear
/// (`None`) one entity's verdict. `kind` is the wire string
/// `"track"`/`"album"`/`"artist"`.
pub async fn set_rating(
    config: &Config,
    kind: &str,
    id: &str,
    rating: Option<&str>,
) -> Result<(), ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, "/v1/library/rating");
    // `rating: null` is a clear — serde_json renders `None` as JSON null.
    let body = serde_json::json!({ "kind": kind, "id": id, "rating": rating });

    let resp = http_client(gw)?
        .put(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .context("sending rating")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("rating rejected ({status}): {text}").into());
    }
    Ok(())
}

/// `GET /v1/library/ratings` — every rated entity for the current user.
pub async fn fetch_ratings(config: &Config) -> Result<Vec<RatingItem>, ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, "/v1/library/ratings");
    let resp: RatingsResponse = http_client(gw)?
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .context("requesting ratings")?
        .error_for_status()
        .context("ratings returned error status")?
        .json()
        .await
        .context("parsing ratings")?;
    Ok(resp.ratings)
}

/// `GET /v1/recommend/next?seed=<id>&n=<n>` — tracks acoustically similar
/// to a seed track, in rank order. 404 → [`ApiError::RecommenderUnavailable`].
pub async fn recommend_next(
    config: &Config,
    seed: &str,
    n: usize,
) -> Result<RecommendList, ApiError> {
    recommend_get(
        config,
        "/v1/recommend/next",
        &[("seed", seed), ("n", &n.to_string())],
        "requesting recommendations",
        "recommend",
    )
    .await
}

/// `GET /v1/recommend/station?text=<prompt>&n=<n>` — a natural-language
/// station, in rank order. 404 → [`ApiError::RecommenderUnavailable`].
pub async fn station(config: &Config, text: &str, n: usize) -> Result<RecommendList, ApiError> {
    recommend_get(
        config,
        "/v1/recommend/station",
        &[("text", text), ("n", &n.to_string())],
        "requesting station",
        "station",
    )
    .await
}

async fn recommend_get(
    config: &Config,
    path: &str,
    query: &[(&str, &str)],
    send_context: &'static str,
    label: &str,
) -> Result<RecommendList, ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, path);
    let resp = http_client(gw)?
        .get(&url)
        .query(query)
        .bearer_auth(&token)
        .send()
        .await
        .context(send_context)?;

    // A 404 here means the recommender is unavailable (no embedder / degraded
    // boot / seed not embedded) rather than a missing route; a 503 is the
    // station path with the embedder down. Neither is a hard error.
    if resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::SERVICE_UNAVAILABLE {
        return Err(ApiError::RecommenderUnavailable);
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("{label} request failed ({status}): {text}").into());
    }
    let parsed: RecommendResponse = resp
        .json()
        .await
        .with_context(|| format!("parsing {label} response"))?;
    Ok(parsed.into())
}

/// `GET /v1/search?query=…` — the gateway's typo-tolerant fuzzy search.
/// The response is a Subsonic `searchResult3` envelope, so it parses with
/// the same wire code as `search3`. `per_kind` caps each result bucket.
pub async fn v1_search(
    config: &Config,
    q: &str,
    per_kind: u32,
) -> Result<SearchResult3, ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, "/v1/search");
    let per_kind = per_kind.to_string();
    let resp = http_client(gw)?
        .get(&url)
        .query(&[
            ("query", q),
            ("artistCount", per_kind.as_str()),
            ("albumCount", per_kind.as_str()),
            ("songCount", per_kind.as_str()),
        ])
        .bearer_auth(&token)
        .send()
        .await
        .context("requesting search")?
        .error_for_status()
        .context("search returned error status")?;
    let body = resp.text().await.context("reading search response")?;
    parse_search_body(&body)
}

fn parse_search_body(body: &str) -> Result<SearchResult3, ApiError> {
    music_subsonic::wire::parse_search3(body)
        .map_err(|e| anyhow!("parsing search response: {e}").into())
}

/// Resolve `ids` to tracks (concurrently, order preserved). Ids that fail to
/// resolve come back separately so a recommender ranking is never silently
/// truncated.
pub async fn resolve_tracks(client: &Client, ids: &[TrackId]) -> (Vec<Track>, Vec<String>) {
    let resolved = join_all(ids.iter().map(|id| client.get_song(id))).await;

    let mut tracks = Vec::new();
    let mut failed = Vec::new();
    for (id, result) in ids.iter().zip(resolved) {
        match result {
            Ok(track) => tracks.push(track),
            Err(_) => failed.push(id.as_str().to_owned()),
        }
    }
    (tracks, failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommend_response_defaults_degraded_to_false() {
        let parsed: RecommendResponse =
            serde_json::from_str(r#"{"results":[{"track_id":"t1"},{"track_id":"t2"}]}"#).unwrap();
        let list = RecommendList::from(parsed);
        assert!(!list.degraded);
        let ids: Vec<_> = list.track_ids.iter().map(TrackId::as_str).collect();
        assert_eq!(ids, ["t1", "t2"]);
    }

    #[test]
    fn recommend_response_reads_degraded_flag() {
        let parsed: RecommendResponse =
            serde_json::from_str(r#"{"degraded":true,"results":[]}"#).unwrap();
        assert!(parsed.degraded);
    }

    #[test]
    fn ratings_response_tolerates_null_rating() {
        let parsed: RatingsResponse = serde_json::from_str(
            r#"{"ratings":[{"kind":"track","id":"t1","rating":"like"},
                           {"kind":"album","id":"a1","rating":null}]}"#,
        )
        .unwrap();
        assert_eq!(parsed.ratings.len(), 2);
        assert_eq!(parsed.ratings[0].rating.as_deref(), Some("like"));
        assert!(parsed.ratings[1].rating.is_none());
    }

    #[test]
    fn search_body_parses_subsonic_envelope() {
        // The gateway's /v1/search answers in the standard Subsonic shape.
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "searchResult3":{
                "artist":[{"id":"ar1","name":"Artist"}],
                "album":[{"id":"al1","name":"Album","songCount":3,"duration":600}],
                "song":[{"id":"t1","title":"Song"}]}}}"#;
        let r = parse_search_body(body).unwrap();
        assert_eq!(r.artists.len(), 1);
        assert_eq!(r.albums.len(), 1);
        assert_eq!(r.tracks.len(), 1);
        assert_eq!(r.tracks[0].title, "Song");
    }

    #[test]
    fn search_body_parse_failure_is_http_error() {
        let err = parse_search_body("not json").unwrap_err();
        assert!(matches!(err, ApiError::Http(_)));
    }
}
