//! HTTP handlers for gateway-owned playlists (`/v1/playlists/*`, PR F).
//!
//! Authorization split:
//!   * **Reads** (`GET`) are any-authenticated. The caller sees their own
//!     playlists plus other users' `shared` ones; a private playlist they
//!     don't own returns **404** (never 403 — we don't leak existence).
//!   * **Writes** (`POST`/`PATCH`/`PUT`/`DELETE`) require the
//!     `WritePlaylist` capability, so a guest gets **403**. Beyond that,
//!     mutation targets must be owned by the caller, else 404.
//!
//! Catalog stays on Navidrome: a playlist holds only track ids, and the
//! client hydrates them against `/rest/*`. So these handlers never call
//! upstream — they're pure gateway-state CRUD.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::store::{PlaylistRow, TrackMode};
use crate::principal::{AuthPrincipal, Capability};
use crate::state::AppState;

/// Upper bound on a playlist name, post-trim. Generous for human names;
/// rejects accidental blob pastes.
const MAX_NAME_LEN: usize = 200;
/// Upper bound on a single membership write. Far above any real playlist;
/// a backstop against a pathological body (the 1 MiB v1 body cap is the
/// other guard).
const MAX_TRACKS: usize = 10_000;

#[derive(Debug, Deserialize)]
pub struct CreateBody {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct PatchBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TracksBody {
    pub track_ids: Vec<String>,
    /// `"replace"` (default) swaps the whole membership — used for reorder
    /// and full set. `"append"` adds after the tail — used for the
    /// row-menu "add to playlist".
    #[serde(default)]
    pub mode: Option<String>,
}

/// Serialize a row to the wire shape. `owned` is relative to the caller so
/// the client can show edit controls only on the caller's own playlists.
fn playlist_json(p: &PlaylistRow, caller: i64) -> Value {
    json!({
        "id": p.id,
        "name": p.name,
        "visibility": p.visibility,
        "owner_user_id": p.owner_user_id,
        "owned": p.owner_user_id == caller,
        "song_count": p.song_count,
        "created_ms": p.created_ms,
        "updated_ms": p.updated_ms,
    })
}

fn validate_name(raw: &str) -> Result<String, (StatusCode, &'static str)> {
    let name = raw.trim();
    if name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "playlist name must not be empty"));
    }
    if name.len() > MAX_NAME_LEN {
        return Err((StatusCode::BAD_REQUEST, "playlist name too long"));
    }
    Ok(name.to_string())
}

fn validate_visibility(raw: &str) -> Result<&'static str, (StatusCode, &'static str)> {
    match raw {
        "private" => Ok("private"),
        "shared" => Ok("shared"),
        _ => Err((StatusCode::BAD_REQUEST, "visibility must be 'private' or 'shared'")),
    }
}

// Taken by value so it drops straight into `.map_err(internal)`; we only
// log it, so clippy flags the unconsumed value — that ergonomics tradeoff
// is deliberate.
#[allow(clippy::needless_pass_by_value)]
fn internal(e: sqlx::Error) -> (StatusCode, &'static str) {
    tracing::error!("playlist store error: {e}");
    (StatusCode::INTERNAL_SERVER_ERROR, "playlist store error")
}

/// Guest/role gate for mutations. Returns 403 for a role without
/// `WritePlaylist` (i.e. guests).
fn require_write(principal: &crate::principal::Principal) -> Result<(), (StatusCode, &'static str)> {
    if principal.can(Capability::WritePlaylist) {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "guests cannot modify playlists"))
    }
}

/// `GET /v1/playlists` — caller's own + others' shared, newest first.
pub async fn list(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
) -> Result<Json<Value>, (StatusCode, &'static str)> {
    let rows = state
        .playlists()
        .list_visible(principal.user_id)
        .await
        .map_err(internal)?;
    let items: Vec<Value> = rows.iter().map(|p| playlist_json(p, principal.user_id)).collect();
    Ok(Json(json!({ "playlists": items })))
}

/// `POST /v1/playlists { name }` — create an empty playlist.
pub async fn create(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Json(body): Json<CreateBody>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    require_write(&principal)?;
    let name = validate_name(&body.name)?;
    let row = state
        .playlists()
        .create(principal.user_id, &name)
        .await
        .map_err(internal)?;
    Ok((StatusCode::CREATED, Json(playlist_json(&row, principal.user_id))))
}

/// `GET /v1/playlists/:id` — full detail (summary + ordered track ids).
/// 404 for a private playlist the caller doesn't own.
pub async fn get(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, &'static str)> {
    let row = load_readable(&state, &principal, &id).await?;
    let track_ids = state.playlists().track_ids(&id).await.map_err(internal)?;
    Ok(Json(json!({
        "playlist": playlist_json(&row, principal.user_id),
        "track_ids": track_ids,
    })))
}

/// `PATCH /v1/playlists/:id { name?, visibility? }`.
pub async fn patch(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path(id): Path<String>,
    Json(body): Json<PatchBody>,
) -> Result<Json<Value>, (StatusCode, &'static str)> {
    require_write(&principal)?;
    load_owned(&state, &principal, &id).await?;

    let name = body.name.as_deref().map(validate_name).transpose()?;
    let visibility = body
        .visibility
        .as_deref()
        .map(validate_visibility)
        .transpose()?;

    let row = state
        .playlists()
        .patch(&id, name.as_deref(), visibility)
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "playlist not found"))?;
    Ok(Json(playlist_json(&row, principal.user_id)))
}

/// `PUT /v1/playlists/:id/tracks { track_ids, mode? }` — set/append/reorder.
pub async fn put_tracks(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path(id): Path<String>,
    Json(body): Json<TracksBody>,
) -> Result<StatusCode, (StatusCode, &'static str)> {
    require_write(&principal)?;
    load_owned(&state, &principal, &id).await?;

    if body.track_ids.len() > MAX_TRACKS {
        return Err((StatusCode::BAD_REQUEST, "too many track ids"));
    }
    if body.track_ids.iter().any(|t| t.trim().is_empty()) {
        return Err((StatusCode::BAD_REQUEST, "track ids must not be empty"));
    }
    let mode = match body.mode.as_deref() {
        None | Some("replace") => TrackMode::Replace,
        Some("append") => TrackMode::Append,
        Some(_) => return Err((StatusCode::BAD_REQUEST, "mode must be 'replace' or 'append'")),
    };

    state
        .playlists()
        .set_tracks(&id, &body.track_ids, mode)
        .await
        .map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/playlists/:id`.
pub async fn delete(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, &'static str)> {
    require_write(&principal)?;
    load_owned(&state, &principal, &id).await?;
    state.playlists().delete(&id).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Load a playlist the caller may **read**: their own, or anyone's
/// `shared`. Otherwise 404 (existence-hiding).
async fn load_readable(
    state: &AppState,
    principal: &crate::principal::Principal,
    id: &str,
) -> Result<PlaylistRow, (StatusCode, &'static str)> {
    let row = state
        .playlists()
        .get(id)
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "playlist not found"))?;
    let readable = row.owner_user_id == principal.user_id || row.visibility == "shared";
    if readable {
        Ok(row)
    } else {
        Err((StatusCode::NOT_FOUND, "playlist not found"))
    }
}

/// Load a playlist the caller **owns** (mutation precondition). A non-owner
/// — even of a `shared` playlist — gets 404, not 403, so a probe can't map
/// out who owns what.
async fn load_owned(
    state: &AppState,
    principal: &crate::principal::Principal,
    id: &str,
) -> Result<PlaylistRow, (StatusCode, &'static str)> {
    let row = state
        .playlists()
        .get(id)
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "playlist not found"))?;
    if row.owner_user_id == principal.user_id {
        Ok(row)
    } else {
        Err((StatusCode::NOT_FOUND, "playlist not found"))
    }
}
