//! `PUT /v1/library/rating` + `GET /v1/library/ratings` — the user's
//! durable per-track like/dislike.
//!
//! This is the gateway's *own* taste store — a deliberate alternative to
//! Subsonic `star`/`unstar`. We never write back to Navidrome (it stays a
//! read-only catalog), so a like/dislike lives only here. It is also a
//! distinct channel from the recommendation thumbs
//! (`/v1/recommend/feedback`): that rates whether a *recommendation* was a
//! good fit (session-scoped, decays); this rates the *song itself*
//! (durable, never decays). See `track_rating.rs` for why they don't merge.
//!
//! Semantics (mirrors the feedback endpoint's nullable-field shape so the
//! client mutation is a single call site):
//! - `{rating: "like"|"dislike"}` UPSERTs the track's verdict.
//! - `{rating: null}` (or omitted) clears it back to neutral (deletes the row).
//!
//! Effects are enforced server-side on every recommend call (dislike =
//! hard-exclude, like = relevance boost) and are always-on — not gated by
//! the preference feature flag.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    response::IntoResponse,
};
use music_core::TrackId;
use music_recommend::Rating;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

fn now_unix_ms() -> i64 {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(dur.as_millis()).unwrap_or(i64::MAX)
}

const MAX_TRACK_ID_LEN: usize = 256;

/// Wire representation of a rating. `None` (the absence of a verdict) is
/// modelled as a nullable field rather than a third enum variant so the
/// PUT body and the GET row share one shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RatingDir {
    Like,
    Dislike,
}

impl From<RatingDir> for Rating {
    fn from(dir: RatingDir) -> Self {
        match dir {
            RatingDir::Like => Rating::Like,
            RatingDir::Dislike => Rating::Dislike,
        }
    }
}

impl From<Rating> for RatingDir {
    fn from(rating: Rating) -> Self {
        match rating {
            Rating::Like => RatingDir::Like,
            Rating::Dislike => RatingDir::Dislike,
        }
    }
}

/// JSON body for `PUT /v1/library/rating`. `rating: None` is a clear.
#[derive(Debug, Deserialize)]
pub struct RatingRequest {
    pub track_id: String,
    #[serde(default)]
    pub rating: Option<RatingDir>,
}

#[derive(Debug, Serialize)]
pub struct RatingItem {
    pub track_id: String,
    /// `None` after a clear (neutral); the like/dislike otherwise.
    pub rating: Option<RatingDir>,
}

#[derive(Debug, Serialize)]
pub struct RatingsResponse {
    pub ratings: Vec<RatingItem>,
}

/// `PUT /v1/library/rating` — set or clear one track's rating.
pub async fn put_rating(
    State(state): State<AppState>,
    payload: Result<Json<RatingRequest>, JsonRejection>,
) -> impl IntoResponse {
    let Ok(Json(req)) = payload else {
        return (StatusCode::BAD_REQUEST, "invalid JSON body").into_response();
    };
    if req.track_id.is_empty() || req.track_id.len() > MAX_TRACK_ID_LEN {
        return (StatusCode::BAD_REQUEST, "track_id length out of range").into_response();
    }

    let track = TrackId::from(req.track_id.as_str());
    let result = match req.rating {
        Some(dir) => state.ratings().set(&track, dir.into(), now_unix_ms()).await,
        None => state.ratings().clear(&track).await,
    };
    if let Err(err) = result {
        tracing::error!(error = %err, "library rating: write failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "rating store unavailable",
        )
            .into_response();
    }

    Json(RatingItem {
        track_id: req.track_id,
        rating: req.rating,
    })
    .into_response()
}

/// `GET /v1/library/ratings` — every rated track, ids + verdict, newest
/// first. Ids only; the client hydrates titles/art (the gateway's
/// `TrackMetadata` lacks cover art, so hydration happens client-side via
/// the Subsonic `getSong` path).
pub async fn list_ratings(State(state): State<AppState>) -> impl IntoResponse {
    match state.ratings().all().await {
        Ok(rows) => Json(RatingsResponse {
            ratings: rows
                .into_iter()
                .map(|(track_id, rating)| RatingItem {
                    track_id: track_id.into_inner(),
                    rating: Some(rating.into()),
                })
                .collect(),
        })
        .into_response(),
        Err(err) => {
            tracing::error!(error = %err, "library ratings: read failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "rating store unavailable",
            )
                .into_response()
        }
    }
}
