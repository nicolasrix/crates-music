//! Admin user provisioning (PR B of the user-system plan).
//!
//! CRUD over real accounts (`admin`/`user`), all under the admin tier in
//! `app.rs` (guarded by `require_admin`), so every handler here may assume
//! the caller is an admin — the route layer already 403'd anyone else.
//!
//! Guests are deliberately out of scope: they have no password and are
//! provisioned by the guest-code redemption flow (PR D), not here.
//! Password material never leaves the gateway — the list endpoint returns
//! `UserSummary` (no hash), and create/reset only ingest a plaintext that
//! is Argon2id-hashed before it touches storage.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::oauth::handlers::MIN_PASSWORD_LEN;
use crate::oauth::password;
use crate::oauth::{Error as OauthError, NewUser};
use crate::principal::OWNER_USER_ID;
use crate::state::AppState;

#[derive(Debug, Serialize)]
struct UserRow {
    id: i64,
    username: Option<String>,
    display_name: Option<String>,
    role: String,
    created_at: i64,
}

/// GET /v1/admin/users — list real accounts (no credential material).
#[tracing::instrument(name = "admin.users.list", skip_all)]
pub async fn list_users(State(state): State<AppState>) -> Result<Json<serde_json::Value>, StatusCode> {
    let users = state.oauth().list_users().await.map_err(|e| {
        tracing::error!(error = %e, "listing users failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let rows: Vec<UserRow> = users
        .into_iter()
        .map(|u| UserRow {
            id: u.id,
            username: u.username,
            display_name: u.display_name,
            role: u.role,
            created_at: u.created_at_unix_ms,
        })
        .collect();
    Ok(Json(json!({ "users": rows })))
}

#[derive(Debug, Deserialize)]
pub struct CreateUserForm {
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
    pub password: String,
    /// `admin` or `user`. `guest` is rejected — guests come from the
    /// guest-code flow (PR D), never the provisioning endpoint.
    pub role: String,
}

/// POST /v1/admin/users — create a real account.
#[tracing::instrument(name = "admin.users.create", skip_all)]
pub async fn create_user(
    State(state): State<AppState>,
    Json(form): Json<CreateUserForm>,
) -> Response {
    let username = form.username.trim();
    if username.is_empty() {
        return bad_request("username must not be empty");
    }
    if !matches!(form.role.as_str(), "admin" | "user") {
        return bad_request("role must be 'admin' or 'user'");
    }
    if form.password.len() < MIN_PASSWORD_LEN {
        return bad_request(&format!(
            "password must be at least {MIN_PASSWORD_LEN} characters"
        ));
    }

    let phc = match password::hash(&form.password) {
        Ok(phc) => phc,
        Err(e) => {
            tracing::error!(error = %e, "argon2 hashing failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let display_name = form
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    match state
        .oauth()
        .create_account(NewUser {
            username: Some(username.to_string()),
            display_name,
            role: form.role.clone(),
            password_hash: Some(phc),
            host_user_id: None,
            expires_at_unix_ms: None,
        })
        .await
    {
        Ok(id) => {
            tracing::info!(user_id = id, role = %form.role, "created user");
            (StatusCode::CREATED, Json(json!({ "id": id }))).into_response()
        }
        Err(OauthError::UsernameTaken) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "username_taken", "message": "username already taken" })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "creating user failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// DELETE /v1/admin/users/:id — remove an account and cascade its
/// tokens/sessions. The owner (id=1) is undeletable: removing it would
/// cascade-delete the owner's data and lock the gateway out of admin.
#[tracing::instrument(name = "admin.users.delete", skip(state))]
pub async fn delete_user(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    if id == OWNER_USER_ID {
        return bad_request("the owner account cannot be deleted");
    }
    match state.oauth().delete_user(id).await {
        Ok(true) => {
            tracing::info!(user_id = id, "deleted user");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "deleting user failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ResetPasswordForm {
    pub password: String,
}

/// POST /v1/admin/users/:id/password — admin-driven password reset (D8:
/// account recovery without email). Rewrites the hash in place, so the
/// account keeps its id and all dependent data.
///
/// The owner (id=1) is exempt: its credential is only resettable via the
/// offline CLI `reset-master-password` path. Without this guard any admin
/// could `POST /v1/admin/users/1/password` and take over the owner
/// account — so the block is unconditional, mirroring `delete_user`.
#[tracing::instrument(name = "admin.users.reset_password", skip(state, form))]
pub async fn reset_password(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(form): Json<ResetPasswordForm>,
) -> Response {
    if id == OWNER_USER_ID {
        return bad_request("the owner password can only be reset from the gateway host CLI");
    }
    if form.password.len() < MIN_PASSWORD_LEN {
        return bad_request(&format!(
            "password must be at least {MIN_PASSWORD_LEN} characters"
        ));
    }
    let phc = match password::hash(&form.password) {
        Ok(phc) => phc,
        Err(e) => {
            tracing::error!(error = %e, "argon2 hashing failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    match state.oauth().set_user_password(id, &phc).await {
        Ok(true) => {
            // Account recovery assumes the account may be compromised, so
            // cut off everything it holds (sec review 1.4): all browser
            // sessions + all refresh/access tokens. Best-effort — the
            // password is already rotated; log but don't fail the reset if
            // revocation errors (leaving stale tokens that expire on TTL).
            if let Err(e) = state.oauth().revoke_all_sessions_for_user(id).await {
                tracing::error!(error = %e, user_id = id, "reset: revoke sessions failed");
            }
            if let Err(e) = state.oauth().revoke_all_tokens_for_user(id).await {
                tracing::error!(error = %e, user_id = id, "reset: revoke tokens failed");
            }
            tracing::info!(user_id = id, "reset user password + revoked sessions/tokens");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "resetting password failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn bad_request(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": "bad_request", "message": message })),
    )
        .into_response()
}
