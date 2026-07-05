//! Gateway recommender fetchers: seed-based (`next`), text stations, album
//! stations (`from-any`), and the album-detail "you might like" groups
//! (`similar_albums` / `similar_artists`). All share the "404/503 =
//! recommender not ready" convention with [`super::ApiError`].

use anyhow::{Context, anyhow};
use reqwest::StatusCode;
use serde::Deserialize;

use super::{ApiError, RecommendList, RecommendResponse};
use crate::config::Config;
use crate::gateway::{endpoint, http_client, require_gateway};

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

/// `POST /v1/recommend/from-any` — pick the best-indexed seed from a set of
/// candidate track ids (e.g. an album's tracks) and return acoustically
/// similar tracks in rank order. Drives "start a station from this album".
/// 404/503 → [`ApiError::RecommenderUnavailable`].
pub async fn recommend_from_any(
    config: &Config,
    candidate_seeds: &[String],
    n: usize,
) -> Result<RecommendList, ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, "/v1/recommend/from-any");
    let body = serde_json::json!({ "candidate_seeds": candidate_seeds, "n": n });
    let resp = http_client(gw)?
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .context("requesting album station")?;
    if resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::SERVICE_UNAVAILABLE {
        return Err(ApiError::RecommenderUnavailable);
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("album station failed ({status}): {text}").into());
    }
    // FromAnyResponse carries extra `seed_used`/`model_version` fields serde
    // ignores; its `degraded`/`results` shape matches RecommendResponse.
    let parsed: RecommendResponse = resp.json().await.context("parsing album station")?;
    Ok(parsed.into())
}

/// The tethered-drift autoplay request: weighted seeds plus the leash / MMR
/// knobs the gateway applies server-side. Assembled by the effect layer from
/// the pure seed builder's output + the `[tui.autoplay]` config.
#[derive(Debug, Clone)]
pub struct WeightedStation<'a> {
    /// `(track_id, weight)` pairs, highest weight first (the builder sorts).
    pub seeds: &'a [(String, f32)],
    /// The leash anchor set (session anchor + user/scrobble seeds, no
    /// frontier) — candidates straying past τ from all of these are demoted.
    pub anchor_track_ids: &'a [String],
    /// Full queue for the diversity walk's context (artist cap, title dedup).
    pub queue_track_ids: &'a [String],
    pub now_playing_track_id: Option<&'a str>,
    /// Recommend-session id — scopes downvote exclusion to this listen.
    pub session_id: Option<&'a str>,
    pub n: usize,
    pub leash_tau: f32,
    pub leash_lambda: f32,
    pub mmr_lambda: f32,
}

/// `POST /v1/recommend/from-seeds` with the full weighted-drift body (the
/// autoplay refill call). Unlike the other recommend endpoints, from-seeds
/// never 404s — an unindexed seed set just yields an empty 200 (surfaced as
/// an empty `RecommendList`, which the caller reads as under-delivery).
pub async fn recommend_weighted_station(
    config: &Config,
    req: &WeightedStation<'_>,
) -> Result<RecommendList, ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, "/v1/recommend/from-seeds");
    let seeds: Vec<&str> = req.seeds.iter().map(|(id, _)| id.as_str()).collect();
    let weights: Vec<f32> = req.seeds.iter().map(|(_, w)| *w).collect();
    let mut body = serde_json::json!({
        "seeds": seeds,
        "seed_weights": weights,
        "top_n": req.n,
        "anchor_track_ids": req.anchor_track_ids,
        "leash_tau": req.leash_tau,
        "leash_lambda": req.leash_lambda,
        "queue_context": {
            "queue_track_ids": req.queue_track_ids,
            "now_playing_track_id": req.now_playing_track_id,
            "diversity_mode": "mmr",
            "mmr_lambda": req.mmr_lambda,
        },
    });
    if let Some(sid) = req.session_id {
        body["session_id"] = serde_json::Value::String(sid.to_owned());
    }
    let resp = http_client(gw)?
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .context("requesting autoplay refill")?;
    if resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::SERVICE_UNAVAILABLE {
        return Err(ApiError::RecommenderUnavailable);
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("autoplay refill failed ({status}): {text}").into());
    }
    let parsed: RecommendResponse = resp.json().await.context("parsing autoplay refill")?;
    Ok(parsed.into())
}

/// Fresh up/down vote totals for a track after a feedback write.
#[derive(Debug, Clone, Deserialize)]
pub struct FeedbackTotals {
    pub up: i64,
    pub down: i64,
}

/// `POST /v1/recommend/feedback` — thumbs up/down on an autoplay-recommended
/// track. `vote = None` clears an existing vote. The gateway upserts one row
/// per `(track_id, session_id)`, so flipping up↔down never double-counts.
/// Any-authenticated (a guest's vote is a silent server-side no-op).
pub async fn submit_feedback(
    config: &Config,
    track_id: &str,
    vote: Option<&str>,
    session_id: &str,
    occurred_ms: i64,
) -> Result<FeedbackTotals, ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, "/v1/recommend/feedback");
    // `vote: null` deletes the row — serde renders `None` as JSON null.
    let body = serde_json::json!({
        "track_id": track_id,
        "vote": vote,
        "session_id": session_id,
        "occurred_ms": occurred_ms,
    });
    let resp = http_client(gw)?
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .context("submitting feedback")?;
    if resp.status() == StatusCode::FORBIDDEN {
        return Err(ApiError::Forbidden);
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("feedback failed ({status}): {text}").into());
    }
    resp.json()
        .await
        .context("parsing feedback response")
        .map_err(Into::into)
}

/// One "you might like" group from `similar_albums`/`similar_artists`: the
/// grouped entity id (an album id or artist id) in descending score order.
/// Names are hydrated by the caller (`getAlbum`/`getArtist`).
#[derive(Debug, Clone)]
pub struct SimilarGroup {
    pub id: String,
    pub score: f32,
}

/// Whether every seed was unindexed (an ingest gap, distinct from "no
/// neighbours") plus the ranked group ids.
#[derive(Debug, Clone)]
pub struct SimilarList {
    pub all_seeds_unindexed: bool,
    pub groups: Vec<SimilarGroup>,
}

#[derive(Debug, Deserialize)]
struct SimilarAlbumItem {
    album_id: String,
    score: f32,
}

#[derive(Debug, Deserialize)]
struct SimilarAlbumsResponse {
    #[serde(default)]
    all_seeds_unindexed: bool,
    results: Vec<SimilarAlbumItem>,
}

#[derive(Debug, Deserialize)]
struct SimilarArtistItem {
    artist_id: String,
    score: f32,
}

#[derive(Debug, Deserialize)]
struct SimilarArtistsResponse {
    #[serde(default)]
    all_seeds_unindexed: bool,
    results: Vec<SimilarArtistItem>,
}

/// `POST /v1/recommend/similar_albums` — albums whose tracks are acoustically
/// near the seed track set, grouped + summed, minus `exclude_album_ids`
/// (typically the seed album itself). 404/503 → `RecommenderUnavailable`.
pub async fn similar_albums(
    config: &Config,
    seed_track_ids: &[String],
    exclude_album_ids: &[String],
    n: usize,
) -> Result<SimilarList, ApiError> {
    let resp: SimilarAlbumsResponse = similar_post(
        config,
        "/v1/recommend/similar_albums",
        serde_json::json!({
            "seed_track_ids": seed_track_ids,
            "exclude_album_ids": exclude_album_ids,
            "n": n,
        }),
    )
    .await?;
    Ok(SimilarList {
        all_seeds_unindexed: resp.all_seeds_unindexed,
        groups: resp
            .results
            .into_iter()
            .map(|r| SimilarGroup {
                id: r.album_id,
                score: r.score,
            })
            .collect(),
    })
}

/// `POST /v1/recommend/similar_artists` — the artist analogue of
/// [`similar_albums`].
pub async fn similar_artists(
    config: &Config,
    seed_track_ids: &[String],
    exclude_artist_ids: &[String],
    n: usize,
) -> Result<SimilarList, ApiError> {
    let resp: SimilarArtistsResponse = similar_post(
        config,
        "/v1/recommend/similar_artists",
        serde_json::json!({
            "seed_track_ids": seed_track_ids,
            "exclude_artist_ids": exclude_artist_ids,
            "n": n,
        }),
    )
    .await?;
    Ok(SimilarList {
        all_seeds_unindexed: resp.all_seeds_unindexed,
        groups: resp
            .results
            .into_iter()
            .map(|r| SimilarGroup {
                id: r.artist_id,
                score: r.score,
            })
            .collect(),
    })
}

/// Shared POST + 404/503-as-unavailable handling for the two `similar_*`
/// endpoints; deserializes the response into the caller's struct.
async fn similar_post<T: serde::de::DeserializeOwned>(
    config: &Config,
    path: &str,
    body: serde_json::Value,
) -> Result<T, ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, path);
    let resp = http_client(gw)?
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .context("requesting similar")?;
    if resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::SERVICE_UNAVAILABLE {
        return Err(ApiError::RecommenderUnavailable);
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("similar request failed ({status}): {text}").into());
    }
    resp.json()
        .await
        .context("parsing similar response")
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_core::TrackId;

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
    fn similar_albums_response_maps_ids_and_scores_in_order() {
        let parsed: SimilarAlbumsResponse = serde_json::from_str(
            r#"{"model_version":"clamp3","all_seeds_unindexed":false,"results":[
                {"album_id":"al2","score":0.91,"supporting_tracks":4},
                {"album_id":"al7","score":0.72,"supporting_tracks":2}]}"#,
        )
        .unwrap();
        assert!(!parsed.all_seeds_unindexed);
        let ids: Vec<_> = parsed.results.iter().map(|r| r.album_id.as_str()).collect();
        assert_eq!(ids, ["al2", "al7"]);
        assert!((parsed.results[0].score - 0.91).abs() < 1e-6);
    }

    #[test]
    fn similar_artists_response_defaults_unindexed_flag() {
        // Older/empty responses may omit all_seeds_unindexed → defaults false.
        let parsed: SimilarArtistsResponse =
            serde_json::from_str(r#"{"results":[{"artist_id":"ar1","score":0.5}]}"#).unwrap();
        assert!(!parsed.all_seeds_unindexed);
        assert_eq!(parsed.results[0].artist_id, "ar1");
    }
}
