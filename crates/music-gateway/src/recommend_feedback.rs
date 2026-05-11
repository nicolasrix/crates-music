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

    let track = TrackId::from(req.track_id.as_str());
    let now_ms = now_unix_ms();
    let occurred_ms = req.occurred_ms.unwrap_or(now_ms);

    let result = match req.vote {
        Some(dir) => {
            state
                .feedback()
                .record(&track, &req.session_id, dir.to_i8(), occurred_ms, now_ms)
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

    let counts = match state.feedback().for_track(&track).await {
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
