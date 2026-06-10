//! `POST /v1/recommend/feedback` — capture thumb-up / thumb-down on a
//! played recommendation.
//!
//! Semantics:
//! - `{vote: "up"|"down"}` UPSERTs the (track_id, session_id) row.
//! - `{vote: null}` deletes the row (the user un-clicked an active
//!   thumb). The HTTP body is the same shape for all three states,
//!   which keeps the client mutation a single call site.
//!
//! Response carries the fresh `(up, down)` totals for the track so
//! the player can render the new counts without a follow-up GET.
//!
//! Session attribution: the client supplies a `session_id`. We trust
//! it — single-tenant gateway, and votes are advisory signal, not
//! security-relevant. A hostile client could spam votes under
//! rotating session ids; that's a problem for another phase.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    response::IntoResponse,
};
use music_core::TrackId;
use serde::{Deserialize, Serialize};

use crate::principal::{AuthPrincipal, Role};
use crate::state::AppState;

fn now_unix_ms() -> i64 {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(dur.as_millis()).unwrap_or(i64::MAX)
}

/// JSON body shape. `vote: None` is a delete; tagged variants would be
/// noisier on the wire than a nullable field.
#[derive(Debug, Deserialize)]
pub struct FeedbackRequest {
    pub track_id: String,
    /// `"up"` → +1, `"down"` → -1, `null` (or omitted) → delete.
    #[serde(default)]
    pub vote: Option<VoteDir>,
    pub session_id: String,
    /// Client-supplied wall-clock millis. Optional — if absent we use
    /// the gateway clock. Keeping the field lets clients backfill
    /// offline votes without losing the listening-context time.
    #[serde(default)]
    pub occurred_ms: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VoteDir {
    Up,
    Down,
}

impl VoteDir {
    fn to_i8(self) -> i8 {
        match self {
            Self::Up => 1,
            Self::Down => -1,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct FeedbackResponse {
    pub track_id: String,
    pub up: i64,
    pub down: i64,
}

const MAX_TRACK_ID_LEN: usize = 256;
const MAX_SESSION_ID_LEN: usize = 128;

pub async fn submit(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    payload: Result<Json<FeedbackRequest>, JsonRejection>,
) -> impl IntoResponse {
    let Ok(Json(req)) = payload else {
        return (StatusCode::BAD_REQUEST, "invalid JSON body").into_response();
    };
    if req.track_id.is_empty() || req.track_id.len() > MAX_TRACK_ID_LEN {
        return (StatusCode::BAD_REQUEST, "track_id length out of range").into_response();
    }
    if req.session_id.is_empty() || req.session_id.len() > MAX_SESSION_ID_LEN {
        return (StatusCode::BAD_REQUEST, "session_id length out of range").into_response();
    }

    // Guest taste sandboxing (PR E): a thumb vote folds into the host's
    // affinity counter, so guest votes are dropped (no write) to keep them
    // from reshaping anyone's taste. Echo zeroed counts so the player UI
    // doesn't error.
    if principal.role == Role::Guest {
        return Json(FeedbackResponse {
            track_id: req.track_id,
            up: 0,
            down: 0,
        })
        .into_response();
    }

    let track = TrackId::from(req.track_id.as_str());
    let now_ms = now_unix_ms();
    let occurred_ms = req.occurred_ms.unwrap_or(now_ms);

    let result = match req.vote {
        Some(dir) => {
            state
                .feedback()
                .record(
                    principal.user_id,
                    &track,
                    &req.session_id,
                    dir.to_i8(),
                    occurred_ms,
                    now_ms,
                )
                .await
        }
        None => state.feedback().clear(&track, &req.session_id).await,
    };
    if let Err(err) = result {
        tracing::error!(error = %err, "feedback: write failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "feedback store unavailable",
        )
            .into_response();
    }

    // Fold the explicit thumb into the preference affinity counter
    // (best-effort; never block the response on it). A `null` vote
    // (un-click) is left to decay rather than un-folded — exact reversal
    // of a decayed counter isn't well-defined, and the signal fades on
    // its own. The feedback endpoint is the sole affinity channel for
    // explicit thumbs; `/v1/events` like/unlike are not double-counted.
    if let Some(dir) = req.vote {
        let event = match dir {
            VoteDir::Up => music_recommend::AffinityEvent::Like,
            VoteDir::Down => music_recommend::AffinityEvent::Dislike,
        };
        if let Err(err) = state
            .track_affinity()
            .apply_event(
                principal.user_id,
                &track,
                event,
                occurred_ms,
                state.affinity_half_life_ms(),
            )
            .await
        {
            tracing::warn!(error = %err, "feedback: affinity update failed; continuing");
        }
    }

    let counts = match state.feedback().for_track(principal.user_id, &track).await {
        Ok(c) => c,
        Err(err) => {
            tracing::error!(error = %err, "feedback: aggregate read failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "feedback store unavailable",
            )
                .into_response();
        }
    };

    Json(FeedbackResponse {
        track_id: req.track_id,
        up: counts.up,
        down: counts.down,
    })
    .into_response()
}
