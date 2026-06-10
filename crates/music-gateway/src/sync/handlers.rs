//! Axum handlers for the sync REST surface.
//!
//! - `GET /v1/sync/snapshot` → current full [`SyncState`] as JSON.
//! - `POST /v1/sync/ops`     → apply a single [`SyncOp`], return the
//!   new version, or `422` with an `{error: …}` body on reject.
//!
//! Bearer auth is applied at the router layer, not here; the injected
//! [`Principal`](crate::principal::Principal) selects the **room** —
//! every read/write is scoped to `principal.room_id()`, so each User
//! gets a private queue and a guest shares their host's (PR C/D).

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use music_sync::{ApplyError, SyncOp, SyncState};
use serde::Serialize;
use serde_json::json;

use crate::principal::AuthPrincipal;
use crate::state::AppState;

pub async fn snapshot(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
) -> Json<SyncState> {
    Json(state.sync().snapshot(principal.room_id()).await)
}

#[derive(Debug, Serialize)]
pub struct OpAck {
    pub version: u64,
}

pub async fn submit_op(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    op: Result<Json<SyncOp>, JsonRejection>,
) -> Response {
    let Json(op) = match op {
        Ok(op) => op,
        Err(rej) => {
            // axum's JsonRejection covers malformed JSON, missing
            // content-type, and shape mismatches. All are client errors.
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": rej.body_text()})),
            )
                .into_response();
        }
    };
    match state.sync().apply(principal.room_id(), &op).await {
        Ok(version) => (StatusCode::OK, Json(OpAck { version })).into_response(),
        Err(ApplyError::NowPlayingOutOfBounds { .. }) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": "now_playing index out of bounds"})),
        )
            .into_response(),
        Err(ApplyError::StartSessionEmpty) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": "start_session items must not be empty"})),
        )
            .into_response(),
    }
}
