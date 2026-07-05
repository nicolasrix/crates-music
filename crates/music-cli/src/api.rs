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

// ── gateway-owned playlists (`/v1/playlists/*`) ──────────────────────────
//
// Playlists live in the gateway, not Navidrome: a playlist stores only an
// ordered list of Navidrome track ids, so a read is a two-step —
// [`get_playlist`] returns the ids, and the caller hydrates them to `Track`s
// via [`resolve_tracks`]. Membership edits (remove/reorder) MUST replace
// against the raw `track_ids`, never a hydrated subset, or an id that failed
// to resolve this load would be silently dropped from the stored playlist.

/// One playlist row from `/v1/playlists`. Deserializes from both the list
/// items and the single-playlist create/patch responses (the wire shape is
/// the same `playlist_json`; unknown fields like `owner_user_id`/timestamps
/// are ignored).
#[derive(Debug, Clone, Deserialize)]
pub struct PlaylistSummary {
    pub id: String,
    pub name: String,
    /// `"private"` / `"shared"`. Defaulted so an older gateway that omits it
    /// doesn't fail the whole parse.
    #[serde(default)]
    pub visibility: String,
    /// `true` when the caller owns this playlist — only owners may edit it.
    #[serde(default)]
    pub owned: bool,
    pub song_count: u32,
}

#[derive(Debug, Deserialize)]
struct PlaylistsResponse {
    playlists: Vec<PlaylistSummary>,
}

#[derive(Debug, Deserialize)]
struct PlaylistDetailResponse {
    playlist: PlaylistSummary,
    track_ids: Vec<String>,
}

/// A playlist plus its raw, ordered track ids. Hydration is the caller's
/// job; edits replace against `track_ids` (see the module note above).
#[derive(Debug, Clone)]
pub struct PlaylistDetail {
    pub summary: PlaylistSummary,
    pub track_ids: Vec<String>,
}

/// Send a request, mapping a 403 to [`ApiError::Forbidden`] and any other
/// non-2xx to a `Http` error carrying the server's message text. Success
/// returns the raw response for the caller to parse (or ignore, for 204s).
async fn send_checked(
    req: reqwest::RequestBuilder,
    ctx: &'static str,
) -> Result<reqwest::Response, ApiError> {
    let resp = req.send().await.context(ctx)?;
    let status = resp.status();
    if status == StatusCode::FORBIDDEN {
        return Err(ApiError::Forbidden);
    }
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("{ctx} failed ({status}): {text}").into());
    }
    Ok(resp)
}

/// `GET /v1/playlists` — the caller's own playlists plus others' `shared`
/// ones, newest first (server order preserved).
pub async fn list_playlists(config: &Config) -> Result<Vec<PlaylistSummary>, ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, "/v1/playlists");
    let resp = send_checked(
        http_client(gw)?.get(&url).bearer_auth(&token),
        "requesting playlists",
    )
    .await?;
    let body: PlaylistsResponse = resp.json().await.context("parsing playlists")?;
    Ok(body.playlists)
}

/// `GET /v1/playlists/:id` — summary + ordered track ids. 404 (mapped to a
/// `Http` error) for a private playlist the caller doesn't own.
pub async fn get_playlist(config: &Config, id: &str) -> Result<PlaylistDetail, ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    // Playlist ids are gateway-generated 128-bit lowercase hex — always
    // URL-safe, so the id drops straight into the path unencoded.
    let url = endpoint(gw, &format!("/v1/playlists/{id}"));
    let resp = send_checked(
        http_client(gw)?.get(&url).bearer_auth(&token),
        "requesting playlist",
    )
    .await?;
    let body: PlaylistDetailResponse = resp.json().await.context("parsing playlist")?;
    Ok(PlaylistDetail {
        summary: body.playlist,
        track_ids: body.track_ids,
    })
}

/// `POST /v1/playlists { name }` — create an empty playlist, returning it.
pub async fn create_playlist(config: &Config, name: &str) -> Result<PlaylistSummary, ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, "/v1/playlists");
    let body = serde_json::json!({ "name": name });
    let resp = send_checked(
        http_client(gw)?.post(&url).bearer_auth(&token).json(&body),
        "creating playlist",
    )
    .await?;
    let summary: PlaylistSummary = resp.json().await.context("parsing created playlist")?;
    Ok(summary)
}

/// `PATCH /v1/playlists/:id { name }` — rename. Owner-only (403 → Forbidden).
pub async fn rename_playlist(config: &Config, id: &str, name: &str) -> Result<(), ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, &format!("/v1/playlists/{id}"));
    let body = serde_json::json!({ "name": name });
    send_checked(
        http_client(gw)?.patch(&url).bearer_auth(&token).json(&body),
        "renaming playlist",
    )
    .await?;
    Ok(())
}

/// `DELETE /v1/playlists/:id`. Owner-only (403 → Forbidden).
pub async fn delete_playlist(config: &Config, id: &str) -> Result<(), ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, &format!("/v1/playlists/{id}"));
    send_checked(
        http_client(gw)?.delete(&url).bearer_auth(&token),
        "deleting playlist",
    )
    .await?;
    Ok(())
}

/// `PUT /v1/playlists/:id/tracks { track_ids, mode }` — set the membership.
/// `append = true` adds after the tail (the row-menu "add to playlist");
/// `append = false` replaces the whole membership (reorder / remove — pass
/// the full desired id list). Owner-only (403 → Forbidden).
pub async fn put_playlist_tracks(
    config: &Config,
    id: &str,
    track_ids: &[String],
    append: bool,
) -> Result<(), ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, &format!("/v1/playlists/{id}/tracks"));
    let mode = if append { "append" } else { "replace" };
    let body = serde_json::json!({ "track_ids": track_ids, "mode": mode });
    send_checked(
        http_client(gw)?.put(&url).bearer_auth(&token).json(&body),
        "updating playlist tracks",
    )
    .await?;
    Ok(())
}

/// Aggregate content recommendations from a set of seed tracks (a playlist's
/// membership) via `POST /v1/recommend/from-seeds`. The gateway samples the
/// seeds, fans out per-seed ANN queries, folds them with Σ-similarity, and
/// excludes the seed set automatically. `all_seeds_unindexed` lets the UI
/// tell "playlist isn't embedded yet" from "nothing more to suggest".
pub async fn suggest_from_seeds(
    config: &Config,
    seeds: &[String],
    n: usize,
) -> Result<SuggestList, ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, "/v1/recommend/from-seeds");
    let body = serde_json::json!({ "seeds": seeds, "top_n": n });
    let resp = http_client(gw)?
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .context("requesting suggestions")?;
    // Same 404/503 = "recommender not ready" convention as the other
    // recommend endpoints.
    if resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::SERVICE_UNAVAILABLE {
        return Err(ApiError::RecommenderUnavailable);
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("suggestions failed ({status}): {text}").into());
    }
    let parsed: FromSeedsResponse = resp.json().await.context("parsing suggestions")?;
    Ok(parsed.into())
}

#[derive(Debug, Deserialize)]
struct FromSeedsResponse {
    #[serde(default)]
    all_seeds_unindexed: bool,
    results: Vec<RecommendItem>,
}

/// Ranked suggestion ids, plus the "every seed was unindexed" signal.
#[derive(Debug, Clone)]
pub struct SuggestList {
    pub all_seeds_unindexed: bool,
    pub track_ids: Vec<TrackId>,
}

impl From<FromSeedsResponse> for SuggestList {
    fn from(resp: FromSeedsResponse) -> Self {
        Self {
            all_seeds_unindexed: resp.all_seeds_unindexed,
            track_ids: resp
                .results
                .into_iter()
                .map(|r| TrackId::from(r.track_id))
                .collect(),
        }
    }
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

    #[test]
    fn playlists_response_parses_and_ignores_extra_fields() {
        // The wire carries owner_user_id + timestamps we don't model — serde
        // ignores unknown fields, so parsing must still succeed.
        let body = r#"{"playlists":[
            {"id":"ab12","name":"Roadtrip","visibility":"shared","owner_user_id":1,
             "owned":true,"song_count":12,"created_ms":1,"updated_ms":2},
            {"id":"cd34","name":"Focus","visibility":"private","owner_user_id":2,
             "owned":false,"song_count":0,"created_ms":3,"updated_ms":4}]}"#;
        let parsed: PlaylistsResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.playlists.len(), 2);
        assert_eq!(parsed.playlists[0].name, "Roadtrip");
        assert!(parsed.playlists[0].owned);
        assert_eq!(parsed.playlists[0].song_count, 12);
        assert!(!parsed.playlists[1].owned);
        assert_eq!(parsed.playlists[1].visibility, "private");
    }

    #[test]
    fn playlist_detail_response_preserves_track_id_order() {
        let body = r#"{"playlist":{"id":"ab12","name":"Roadtrip","visibility":"private",
            "owner_user_id":1,"owned":true,"song_count":3,"created_ms":1,"updated_ms":2},
            "track_ids":["t3","t1","t2"]}"#;
        let parsed: PlaylistDetailResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.playlist.id, "ab12");
        assert_eq!(parsed.track_ids, ["t3", "t1", "t2"]);
    }

    #[test]
    fn playlist_summary_tolerates_missing_visibility() {
        // An older gateway might omit visibility; `#[serde(default)]` keeps
        // the parse from failing.
        let s: PlaylistSummary =
            serde_json::from_str(r#"{"id":"x","name":"n","owned":true,"song_count":1}"#).unwrap();
        assert_eq!(s.visibility, "");
    }

    #[test]
    fn from_seeds_response_defaults_and_maps_to_suggest_list() {
        let parsed: FromSeedsResponse = serde_json::from_str(
            r#"{"model_version":"clamp3","degraded":false,
                "results":[{"track_id":"s1"},{"track_id":"s2"}]}"#,
        )
        .unwrap();
        assert!(!parsed.all_seeds_unindexed);
        let list = SuggestList::from(parsed);
        let ids: Vec<_> = list.track_ids.iter().map(TrackId::as_str).collect();
        assert_eq!(ids, ["s1", "s2"]);

        let unindexed: FromSeedsResponse =
            serde_json::from_str(r#"{"all_seeds_unindexed":true,"results":[]}"#).unwrap();
        assert!(SuggestList::from(unindexed).all_seeds_unindexed);
    }
}
