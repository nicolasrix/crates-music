//! HTTP handlers for the OAuth surface.
//!
//! P3.1.b ships only the bootstrap endpoint. Login, authorize, token, and
//! revoke arrive in subsequent sub-phases.

use axum::Form;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;

use crate::oauth::password;
use crate::state::AppState;

/// Minimum master-password length. NIST SP 800-63B recommends ≥ 8 with
/// no other rules; we go a little stricter (this is the *root* credential).
pub const MIN_PASSWORD_LEN: usize = 12;

#[derive(Debug, Deserialize)]
pub struct SetupForm {
    pub token: String,
    pub password: String,
}

/// POST /oauth/setup
///
/// One-shot bootstrap. Accepts the setup token printed at startup plus a
/// master password; hashes the password (Argon2id) and stores it. Once a
/// master password exists, the endpoint returns 410 Gone forever.
pub async fn setup(
    State(state): State<AppState>,
    Form(form): Form<SetupForm>,
) -> Result<StatusCode, (StatusCode, String)> {
    // Already configured → endpoint is permanently disabled, regardless
    // of whether the in-memory token is still set.
    if state
        .oauth()
        .master_password_hash()
        .await
        .map_err(internal)?
        .is_some()
    {
        return Err((
            StatusCode::GONE,
            "gateway is already configured".to_string(),
        ));
    }

    // No active token → never was one, or it's already been consumed.
    if !state.setup_token().is_active() {
        return Err((StatusCode::GONE, "setup token is not active".to_string()));
    }

    if !state.setup_token().matches(&form.token) {
        return Err((StatusCode::FORBIDDEN, "invalid setup token".to_string()));
    }

    if form.password.len() < MIN_PASSWORD_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("password must be at least {MIN_PASSWORD_LEN} characters"),
        ));
    }

    let phc = password::hash(&form.password).map_err(|e| {
        tracing::error!("argon2 hashing failed: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not hash password".to_string(),
        )
    })?;
    state
        .oauth()
        .set_master_password_hash(&phc)
        .await
        .map_err(internal)?;

    // Burn the token only on success — a failed attempt shouldn't lock
    // the operator out.
    let _ = state.setup_token().consume();

    Ok(StatusCode::OK)
}

fn internal<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    tracing::error!("oauth setup: {e}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal error".to_string(),
    )
}
