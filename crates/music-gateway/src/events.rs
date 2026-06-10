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
use music_recommend::{AffinityEvent, EventInput, EventType};
use serde::{Deserialize, Serialize};

use crate::principal::{AuthPrincipal, Role};
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
    AuthPrincipal(principal): AuthPrincipal,
    payload: Result<Json<EventsRequest>, JsonRejection>,
) -> impl IntoResponse {
    let Ok(Json(req)) = payload else {
        return (StatusCode::BAD_REQUEST, "invalid JSON body").into_response();
    };
    // Guest taste sandboxing: a guest is a transient participant in a
    // host's room (PR D) and must never reshape anyone's taste, so their
    // events are dropped from training entirely (capability table, PR E).
    // We accept the request (so the player's fire-and-forget batcher
    // doesn't error) but persist nothing and fold no affinity.
    if principal.role == Role::Guest {
        return (StatusCode::ACCEPTED, Json(EventsResponse { accepted: 0 })).into_response();
    }
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

    match state
        .event_store()
        .append_batch(principal.user_id, &events)
        .await
    {
        Ok(n) => {
            // Durable capture done. Now fold skips into the preference
            // affinity counter (best-effort, post-persist so a failure
            // here never costs us the logged signal).
            fold_skip_affinity(&state, principal.user_id, &events).await;
            (StatusCode::ACCEPTED, Json(EventsResponse { accepted: n })).into_response()
        }
        Err(err) => {
            tracing::error!(error = %err, "events: batch append failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "event log unavailable").into_response()
        }
    }
}

/// Pull a `played_ms` (playback position at skip) out of an event's
/// opaque metadata. Accepts integer or float JSON; `None` if absent or
/// the wrong shape.
#[allow(clippy::cast_possible_truncation)] // playback position in ms; precision loss is immaterial
fn extract_played_ms(metadata: Option<&serde_json::Value>) -> Option<i64> {
    let v = metadata?.get("played_ms")?;
    v.as_i64()
        .or_else(|| v.as_f64().map(|f| f as i64))
        .filter(|ms| *ms >= 0)
}

/// Fold each skip in the batch into the affinity counter, scaled by how
/// far into the track the user got (`played_ms / duration`). A skip
/// without a known position *or* a track of unknown duration is left
/// alone — we don't penalise blindly, since an early-vs-late skip means
/// very different things and we can't tell them apart without both.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // completion ratio in [0,1]
async fn fold_skip_affinity(state: &AppState, user_id: i64, events: &[EventInput]) {
    let half_life_ms = state.affinity_half_life_ms();
    for ev in events {
        if ev.event_type != EventType::Skip {
            continue;
        }
        let Some(played_ms) = extract_played_ms(ev.metadata.as_ref()) else {
            continue;
        };
        let duration_ms = match state.metadata_store().get(&ev.track_id).await {
            Ok(Some(meta)) => meta.duration_seconds.map(|s| i64::from(s) * 1000),
            _ => None,
        };
        let Some(duration_ms) = duration_ms.filter(|d| *d > 0) else {
            continue;
        };
        // clamp handled inside event_weight; compute the raw ratio here.
        let completion = (played_ms as f64 / duration_ms as f64) as f32;
        if let Err(err) = state
            .track_affinity()
            .apply_event(
                user_id,
                &ev.track_id,
                AffinityEvent::Skip { completion },
                ev.occurred_at,
                half_life_ms,
            )
            .await
        {
            tracing::warn!(error = %err, "events: skip affinity update failed; continuing");
        }
    }
}
