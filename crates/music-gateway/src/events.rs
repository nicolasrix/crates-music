//! `POST /v1/events` — append-only event log endpoint.
//!
//! Clients batch user-interaction events (scrobble, skip, like,
//! seek, …) and POST them every few seconds or on app background.
//! No consumers yet — that arrives with the behavioural-similarity
//! index in a later phase. The point is to capture the signal so it
//! isn't lost.

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    response::IntoResponse,
};
use music_recommend::{EventInput, EventType};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// Cap on a single batch. Higher than this is almost always a bug
/// (clients should coalesce into smaller windows). Keeps a single
/// request from running unbounded transactions.
const MAX_BATCH: usize = 1_000;

#[derive(Debug, Deserialize)]
pub struct EventsRequest {
    pub events: Vec<EventPayload>,
}

#[derive(Debug, Deserialize)]
pub struct EventPayload {
    pub event_type: EventType,
    pub track_id: String,
    pub occurred_at: i64,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct EventsResponse {
    pub accepted: u64,
}

pub async fn submit(
    State(state): State<AppState>,
    payload: Result<Json<EventsRequest>, JsonRejection>,
) -> impl IntoResponse {
    let Ok(Json(req)) = payload else {
        return (StatusCode::BAD_REQUEST, "invalid JSON body").into_response();
    };
    if req.events.is_empty() {
        return (StatusCode::BAD_REQUEST, "events array must be non-empty").into_response();
    }
    if req.events.len() > MAX_BATCH {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            "batch too large (max 1000 events)",
        )
            .into_response();
    }

    let events: Vec<EventInput> = req
        .events
        .into_iter()
        .map(|p| EventInput {
            event_type: p.event_type,
            track_id: music_core::TrackId::from(p.track_id),
            occurred_at: p.occurred_at,
            metadata: p.metadata,
        })
        .collect();

    match state.event_store().append_batch(&events).await {
        Ok(n) => (StatusCode::ACCEPTED, Json(EventsResponse { accepted: n })).into_response(),
        Err(err) => {
            tracing::error!(error = %err, "events: batch append failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "event log unavailable").into_response()
        }
    }
}
