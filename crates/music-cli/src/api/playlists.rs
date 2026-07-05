//! Gateway-owned playlist fetchers (`/v1/playlists/*`) plus the playlist
//! "suggest more" call (`/v1/recommend/from-seeds`).
//!
//! Playlists live in the gateway, not Navidrome: a playlist stores only an
//! ordered list of Navidrome track ids, so a read is a two-step —
//! [`get_playlist`] returns the ids, and the caller hydrates them to `Track`s
//! via [`super::resolve_tracks`]. Membership edits (remove/reorder) MUST
//! replace against the raw `track_ids`, never a hydrated subset, or an id
//! that failed to resolve this load would be silently dropped from the
//! stored playlist.

use anyhow::{Context, anyhow};
use music_core::TrackId;
use reqwest::StatusCode;
use serde::Deserialize;

use super::{ApiError, RecommendItem};
use crate::config::Config;
use crate::gateway::{endpoint, http_client, require_gateway};

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

#[cfg(test)]
mod tests {
    use super::*;

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
