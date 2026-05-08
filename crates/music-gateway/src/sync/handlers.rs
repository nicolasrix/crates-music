//! Axum handlers for the sync REST surface.
//!
//! - `GET /v1/sync/snapshot` → current full [`SyncState`] as JSON.
//! - `POST /v1/sync/ops`     → apply a single [`SyncOp`], return the
//!   new version, or `422` with an `{error: …}` body on reject.
//!
//! Bearer auth is applied at the router layer, not here.

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use music_sync::{ApplyError, SyncOp, SyncState};
use serde::Serialize;
use serde_json::json;

use crate::state::AppState;

pub async fn snapshot(State(state): State<AppState>) -> Json<SyncState> {
    Json(state.sync().snapshot().await)
}

#[derive(Debug, Serialize)]
pub struct OpAck {
    pub version: u64,
}

pub async fn submit_op(
    State(state): State<AppState>,
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
    match state.sync().apply(&op).await {
        Ok(version) => (StatusCode::OK, Json(OpAck { version })).into_response(),
        Err(ApplyError::NowPlayingOutOfBounds { .. }) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": "now_playing index out of bounds"})),
        )
            .into_response(),
    }
}
