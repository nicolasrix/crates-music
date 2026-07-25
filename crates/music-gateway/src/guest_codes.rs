//! `POST/GET /v1/guest_codes` + `DELETE /v1/guest_codes/:id` — a host
//! manages the shareable codes that let visitors join their room as a
//! guest (PR D of the user-system plan).
//!
//! A code is always owned by the calling principal (`host_user_id =
//! principal.user_id`), so a User manages *their own* codes and an Admin
//! manages theirs; nobody can touch another host's. Guests can't mint
//! codes (they have no room of their own to invite into), so these
//! handlers 403 a `Role::Guest` even though the route sits in the
//! any-authenticated tier.
//!
//! Redeeming a code is the *un*authenticated `POST /oauth/guest` grant in
//! `oauth::handlers` — this module is only the host-side management.
//!
//! The plaintext code is returned exactly once, by `create`. It is never
//! recoverable afterwards (only its sha256 is stored), so the host must
//! capture it at creation — same contract as every other secret here.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use crate::oauth::{GuestCodeRow, NewGuestCode, OauthStore};
use crate::principal::{AuthPrincipal, Role};
use crate::state::AppState;

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Spawn the background reaper for expired guest accounts. Every
/// `interval` it deletes guest rows past their `expires_at`, cascading
/// their tokens via the schema's `ON DELETE CASCADE`. A `0` interval
/// disables the loop (returns `None`) — lapsed guests are still rejected
/// at auth time, so this is pure housekeeping.
///
/// The handle is detached by the caller (held only so the task isn't
/// dropped); it runs for the process lifetime.
#[must_use]
pub fn spawn_guest_sweep(oauth: OauthStore, interval: Duration) -> Option<JoinHandle<()>> {
    if interval.is_zero() {
        tracing::info!("guest sweep disabled (interval = 0)");
        return None;
    }
    Some(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // Skip the immediate first tick's burst on catch-up after a pause.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let now = now_unix_ms();
            match oauth.delete_expired_guests(now).await {
                Ok(0) => {}
                Ok(n) => tracing::info!("guest sweep: reaped {n} expired guest account(s)"),
                Err(e) => tracing::warn!("guest sweep failed: {e}"),
            }
        }
    }))
}

/// Upper bound on the free-text label.
const MAX_LABEL_LEN: usize = 128;

#[derive(Debug, Deserialize)]
pub struct CreateGuestCodeRequest {
    /// Free-text reminder of what the code is for. Trimmed + capped.
    #[serde(default)]
    pub label: Option<String>,
    /// Relative expiry. `None` (or `0`) = never expires (revoke to kill).
    #[serde(default)]
    pub expires_in_seconds: Option<u64>,
    /// Redemption cap. `None` = unlimited.
    #[serde(default)]
    pub max_uses: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct CreatedGuestCode {
    pub id: i64,
    /// The plaintext code — shown to the host **once**.
    pub code: String,
    pub expires_at_unix_ms: Option<i64>,
    pub max_uses: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct GuestCodeView {
    pub id: i64,
    pub label: Option<String>,
    pub created_at_unix_ms: i64,
    pub expires_at_unix_ms: Option<i64>,
    pub max_uses: Option<i64>,
    pub uses: i64,
    pub revoked_at_unix_ms: Option<i64>,
}

impl From<GuestCodeRow> for GuestCodeView {
    fn from(r: GuestCodeRow) -> Self {
        Self {
            id: r.id,
            label: r.label,
            created_at_unix_ms: r.created_at_unix_ms,
            expires_at_unix_ms: r.expires_at_unix_ms,
            max_uses: r.max_uses,
            uses: r.uses,
            revoked_at_unix_ms: r.revoked_at_unix_ms,
        }
    }
}

fn forbidden_for_guest() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({
            "error": "forbidden",
            "message": "guests cannot manage guest codes",
        })),
    )
}

fn internal(err: &crate::oauth::Error) -> (StatusCode, Json<serde_json::Value>) {
    tracing::error!("guest_codes store error: {err}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": "internal" })),
    )
}

/// POST /v1/guest_codes — mint a code owned by the caller.
pub async fn create(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    body: Result<Json<CreateGuestCodeRequest>, JsonRejection>,
) -> impl IntoResponse {
    if principal.role == Role::Guest {
        return forbidden_for_guest().into_response();
    }
    let Json(req) = match body {
        Ok(b) => b,
        Err(rej) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "invalid_request", "message": rej.body_text() })),
            )
                .into_response();
        }
    };

    let label = req.label.and_then(|l| {
        let t = l.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.chars().take(MAX_LABEL_LEN).collect::<String>())
        }
    });
    // 0 / absent => no expiry. Reject negative max_uses (a 0 cap would mint
    // a dead code, so require >= 1 when present).
    let expires_at_unix_ms = match req.expires_in_seconds {
        Some(secs) if secs > 0 => {
            Some(now_unix_ms().saturating_add(i64::try_from(secs.saturating_mul(1000)).unwrap_or(i64::MAX)))
        }
        _ => None,
    };
    if let Some(max) = req.max_uses
        && max < 1
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid_request", "message": "max_uses must be >= 1" })),
        )
            .into_response();
    }

    match state
        .oauth()
        .create_guest_code(NewGuestCode {
            host_user_id: principal.user_id,
            label,
            expires_at_unix_ms,
            max_uses: req.max_uses,
        })
        .await
    {
        Ok(issued) => (
            StatusCode::CREATED,
            Json(CreatedGuestCode {
                id: issued.id,
                code: issued.code,
                expires_at_unix_ms: issued.expires_at_unix_ms,
                max_uses: issued.max_uses,
            }),
        )
            .into_response(),
        Err(e) => internal(&e).into_response(),
    }
}

/// GET /v1/guest_codes — the caller's codes, newest first.
pub async fn list(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
) -> impl IntoResponse {
    if principal.role == Role::Guest {
        return forbidden_for_guest().into_response();
    }
    match state.oauth().list_guest_codes(principal.user_id).await {
        Ok(rows) => {
            let views: Vec<GuestCodeView> = rows.into_iter().map(GuestCodeView::from).collect();
            (StatusCode::OK, Json(views)).into_response()
        }
        Err(e) => internal(&e).into_response(),
    }
}

/// DELETE /v1/guest_codes/:id — revoke one of the caller's codes.
/// Idempotent: 204 whether or not the row was still live; 404 only when no
/// such code is owned by the caller.
pub async fn revoke(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    if principal.role == Role::Guest {
        return forbidden_for_guest().into_response();
    }
    match state.oauth().revoke_guest_code(principal.user_id, id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        // Either already revoked, or not the caller's. Distinguish so a
        // double-revoke is a clean no-op but a wrong id is a 404.
        Ok(false) => match state.oauth().list_guest_codes(principal.user_id).await {
            Ok(rows) if rows.iter().any(|r| r.id == id) => StatusCode::NO_CONTENT.into_response(),
            Ok(_) => StatusCode::NOT_FOUND.into_response(),
            Err(e) => internal(&e).into_response(),
        },
        Err(e) => internal(&e).into_response(),
    }
}
