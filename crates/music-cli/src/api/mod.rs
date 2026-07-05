//! Typed gateway `/v1` fetchers, shared by the classic printing commands and
//! the interactive TUI.
//!
//! The split: this module does HTTP + parsing and returns data;
//! `ratings.rs` / `recommend.rs` stay as thin printing wrappers so the
//! classic commands' stdout is byte-identical, and the TUI consumes the same
//! typed results without scraping any of it. Auth goes through
//! [`crate::auth::resolve_bearer`] *per request*, so long-lived TUI sessions
//! pick up refreshed tokens naturally.
//!
//! This root module holds the shared error/wire types plus ratings, events,
//! search, whoami, sync-snapshot, and track hydration; the recommender and
//! playlist fetchers live in the [`recommend`] and [`playlists`] submodules
//! (glob-re-exported, so callers still use `api::<name>`).

mod playlists;
mod recommend;

pub use playlists::{
    PlaylistDetail, PlaylistSummary, SuggestList, create_playlist, delete_playlist, get_playlist,
    list_playlists, put_playlist_tracks, rename_playlist, suggest_from_seeds,
};
pub use recommend::{
    SimilarGroup, SimilarList, recommend_from_any, recommend_next, similar_albums, similar_artists,
    station,
};

use anyhow::{Context, anyhow};
use futures_util::future::join_all;
use music_core::{Track, TrackId};
use music_subsonic::{Client, SearchResult3};
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
    /// A write was refused with 403 — the caller's role lacks the capability
    /// (a guest token can read playlists but not modify them). Split out so
    /// callers can render an honest "not permitted" message rather than a
    /// raw HTTP dump (decision D3 in the parity plan).
    #[error("this action isn't permitted for your account")]
    Forbidden,
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

/// One `{track_id}` item, shared by the recommend and playlist-suggest
/// response shapes (both submodules deserialize it).
#[derive(Debug, Deserialize)]
pub(crate) struct RecommendItem {
    pub(crate) track_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RecommendResponse {
    /// True when the recommender fell back to tag-only similarity (seed not
    /// embedded, or the index is still warming up). Stations don't send it.
    #[serde(default)]
    pub(crate) degraded: bool,
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

/// One event for `POST /v1/events` — the gateway's append-only interaction
/// log that feeds per-track preference affinity (today only `skip`, scaled
/// by `played_ms`). Matches the wire shape in
/// `music-gateway/src/events.rs::EventPayload`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OutgoingEvent {
    pub event_type: String,
    pub track_id: String,
    /// Client-stamped unix milliseconds.
    pub occurred_at: i64,
    /// Type-specific blob, e.g. `{"played_ms": …}` for a skip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// `POST /v1/events` — batch-upload interaction events. Callers coalesce
/// (one POST every few seconds), so a failure here loses at most a small
/// window; the TUI re-queues on error.
pub async fn post_events(config: &Config, events: &[OutgoingEvent]) -> Result<(), ApiError> {
    if events.is_empty() {
        return Ok(());
    }
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, "/v1/events");
    let body = serde_json::json!({ "events": events });
    let resp = http_client(gw)?
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .context("sending events")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("events rejected ({status}): {text}").into());
    }
    Ok(())
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

/// Identity of the calling principal, from `GET /v1/whoami` — drives
/// role-gated UI (guests can't write, only admins see diagnostics).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WhoamiInfo {
    pub user_id: i64,
    /// `"admin"` / `"user"` / `"guest"`.
    pub role: String,
    pub username: Option<String>,
    pub display_name: Option<String>,
}

impl WhoamiInfo {
    /// The name to show in a UI corner: display name, else username,
    /// else the numeric principal.
    #[must_use]
    pub fn label(&self) -> String {
        self.display_name
            .clone()
            .or_else(|| self.username.clone())
            .unwrap_or_else(|| format!("user #{}", self.user_id))
    }
}

/// `GET /v1/whoami` — who the gateway thinks we are.
pub async fn whoami(config: &Config) -> Result<WhoamiInfo, ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, "/v1/whoami");
    let info: WhoamiInfo = http_client(gw)?
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .context("requesting whoami")?
        .error_for_status()
        .context("whoami returned error status")?
        .json()
        .await
        .context("parsing whoami")?;
    Ok(info)
}

/// `GET /v1/sync/snapshot` — the room's full sync state, for converging
/// after a missed WS frame without tearing the connection down.
pub async fn sync_snapshot(config: &Config) -> Result<music_sync::SyncState, ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, "/v1/sync/snapshot");
    let state: music_sync::SyncState = http_client(gw)?
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .context("requesting sync snapshot")?
        .error_for_status()
        .context("sync snapshot returned error status")?
        .json()
        .await
        .context("parsing sync snapshot")?;
    Ok(state)
}

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

    #[test]
    fn outgoing_event_serializes_to_the_gateway_wire_shape() {
        // Must deserialize as `music-gateway/src/events.rs::EventPayload`:
        // event_type / track_id / occurred_at, optional metadata blob.
        let with_meta = OutgoingEvent {
            event_type: "skip".to_owned(),
            track_id: "t1".to_owned(),
            occurred_at: 1_700_000_000_000,
            metadata: Some(serde_json::json!({ "played_ms": 61_500 })),
        };
        assert_eq!(
            serde_json::to_value(&with_meta).unwrap(),
            serde_json::json!({
                "event_type": "skip",
                "track_id": "t1",
                "occurred_at": 1_700_000_000_000_i64,
                "metadata": { "played_ms": 61_500 },
            })
        );

        // No metadata → the key is omitted entirely, not null.
        let bare = OutgoingEvent {
            event_type: "scrobble".to_owned(),
            track_id: "t2".to_owned(),
            occurred_at: 1,
            metadata: None,
        };
        let v = serde_json::to_value(&bare).unwrap();
        assert!(v.get("metadata").is_none());
    }
}
