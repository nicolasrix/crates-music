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
use music_core::SessionId;
use music_recommend::{EventInput, EventType};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// Cap on a single batch. Higher than this is almost always a bug
/// (clients should coalesce into smaller windows). Keeps a single
/// request from running unbounded transactions.
const MAX_BATCH: usize = 1_000;

/// Cap on the free-form `EventType::Other` string. Known variants are
/// short words; an unbounded custom type is a buggy/abusive client
/// writing megabytes into the `event_type` column.
const MAX_EVENT_TYPE_LEN: usize = 64;
/// Cap on a track id. Subsonic ids are short opaque strings; anything
/// longer is malformed.
const MAX_TRACK_ID_LEN: usize = 256;
/// Cap on serialized `metadata` JSON per event. Metadata is a small
/// type-specific blob (e.g. `played_ms`); 4 KiB is generous.
const MAX_METADATA_BYTES: usize = 4096;

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
    /// Active recommend-session at the moment the event fired. Optional
    /// for backwards compat — pre-0007 clients omit it and the event
    /// lands with NULL session_id (out-of-session).
    #[serde(default)]
    pub session_id: Option<String>,
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
    // Per-field caps: reject the whole batch rather than write an
    // oversized free-text field into the event log.
    for p in &req.events {
        if p.event_type.as_str().len() > MAX_EVENT_TYPE_LEN {
            return (StatusCode::BAD_REQUEST, "event_type too long").into_response();
        }
        if p.track_id.len() > MAX_TRACK_ID_LEN {
            return (StatusCode::BAD_REQUEST, "track_id too long").into_response();
        }
        if let Some(m) = &p.metadata
            && m.to_string().len() > MAX_METADATA_BYTES
        {
            return (StatusCode::BAD_REQUEST, "metadata too large").into_response();
        }
    }

    let events: Vec<EventInput> = req
        .events
        .into_iter()
        .map(|p| EventInput {
            event_type: p.event_type,
            track_id: music_core::TrackId::from(p.track_id),
            occurred_at: p.occurred_at,
            metadata: p.metadata,
            session_id: p.session_id.map(SessionId::from),
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
