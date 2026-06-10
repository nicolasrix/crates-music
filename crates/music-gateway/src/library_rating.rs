//! `PUT /v1/library/rating` + `GET /v1/library/ratings` — the user's
//! durable like/dislike for any rateable library entity (track, album, or
//! artist).
//!
//! This is the gateway's *own* taste store — a deliberate alternative to
//! Subsonic `star`/`unstar`. We never write back to Navidrome (it stays a
//! read-only catalog), so a like/dislike lives only here. It is also a
//! distinct channel from the recommendation thumbs
//! (`/v1/recommend/feedback`): that rates whether a *recommendation* was a
//! good fit (session-scoped, decays); this rates the *entity* itself
//! (durable, never decays). See `rating.rs` for why they don't merge.
//!
//! Semantics (mirrors the feedback endpoint's nullable-field shape so the
//! client mutation is a single call site):
//! - `{kind, id, rating: "like"|"dislike"}` UPSERTs the entity's verdict.
//! - `{kind, id, rating: null}` (or omitted) clears it back to neutral
//!   (deletes the row).
//!
//! Effects are enforced server-side on every recommend call and are
//! always-on (not gated by the preference feature flag):
//! - **dislike** hard-excludes the entity from play entirely — a disliked
//!   track, or *every track* of a disliked album/artist, is dropped from
//!   all recommender candidate generation and auto-skipped in the player.
//! - **like** boosts relevance, weighted track > album > artist.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    response::IntoResponse,
};
use music_recommend::{RatedKind, Rating};
use serde::{Deserialize, Serialize};

use crate::principal::{AuthPrincipal, Role};
use crate::state::AppState;

fn now_unix_ms() -> i64 {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(dur.as_millis()).unwrap_or(i64::MAX)
}

const MAX_ENTITY_ID_LEN: usize = 256;

/// Wire form of the entity kind. A thin serde mirror of
/// [`music_recommend::RatedKind`] (which is kept serde-free in the domain
/// crate); the lowercase strings match the on-disk `kind` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntityKind {
    Track,
    Album,
    Artist,
}

impl From<EntityKind> for RatedKind {
    fn from(k: EntityKind) -> Self {
        match k {
            EntityKind::Track => RatedKind::Track,
            EntityKind::Album => RatedKind::Album,
            EntityKind::Artist => RatedKind::Artist,
        }
    }
}

impl From<RatedKind> for EntityKind {
    fn from(k: RatedKind) -> Self {
        match k {
            RatedKind::Track => EntityKind::Track,
            RatedKind::Album => EntityKind::Album,
            RatedKind::Artist => EntityKind::Artist,
        }
    }
}

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
/// `kind` defaults to `track` so older single-kind clients (and the
/// track-only wire shape they sent) keep working unchanged.
#[derive(Debug, Deserialize)]
pub struct RatingRequest {
    #[serde(default = "default_kind")]
    pub kind: EntityKind,
    pub id: String,
    #[serde(default)]
    pub rating: Option<RatingDir>,
}

fn default_kind() -> EntityKind {
    EntityKind::Track
}

#[derive(Debug, Serialize)]
pub struct RatingItem {
    pub kind: EntityKind,
    pub id: String,
    /// `None` after a clear (neutral); the like/dislike otherwise.
    pub rating: Option<RatingDir>,
}

#[derive(Debug, Serialize)]
pub struct RatingsResponse {
    pub ratings: Vec<RatingItem>,
}

/// `PUT /v1/library/rating` — set or clear one entity's rating.
pub async fn put_rating(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    payload: Result<Json<RatingRequest>, JsonRejection>,
) -> impl IntoResponse {
    // Ratings are durable, personal taste (capability `WriteTaste`). A
    // guest has no library of their own and must never reshape the host's
    // taste, so writing a rating is forbidden outright (PR E).
    if principal.role == Role::Guest {
        return (StatusCode::FORBIDDEN, "guests cannot rate").into_response();
    }
    let Ok(Json(req)) = payload else {
        return (StatusCode::BAD_REQUEST, "invalid JSON body").into_response();
    };
    if req.id.is_empty() || req.id.len() > MAX_ENTITY_ID_LEN {
        return (StatusCode::BAD_REQUEST, "id length out of range").into_response();
    }

    let kind: RatedKind = req.kind.into();
    let result = match req.rating {
        Some(dir) => {
            state
                .ratings()
                .set(principal.user_id, kind, &req.id, dir.into(), now_unix_ms())
                .await
        }
        None => state.ratings().clear(principal.user_id, kind, &req.id).await,
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
        kind: req.kind,
        id: req.id,
        rating: req.rating,
    })
    .into_response()
}

/// `GET /v1/library/ratings` — every rated entity, kind + id + verdict,
/// newest first. Ids only; the client hydrates titles/art (the gateway's
/// `TrackMetadata` lacks cover art, so hydration happens client-side via
/// the Subsonic `getSong` / `getAlbum` / `getArtist` paths).
pub async fn list_ratings(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
) -> impl IntoResponse {
    match state.ratings().all(principal.user_id).await {
        Ok(rows) => Json(RatingsResponse {
            ratings: rows
                .into_iter()
                .map(|(kind, id, rating)| RatingItem {
                    kind: kind.into(),
                    id,
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
