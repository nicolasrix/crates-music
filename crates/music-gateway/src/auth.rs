//! Bearer-token middleware for protected routes.
//!
//! Accepts either an OAuth-issued access token (looked up by sha256 in
//! the access_tokens table) or the legacy static bearer from config.
//! Constant-time comparison on the static-bearer path so timing doesn't
//! leak the token.

use axum::{
    extract::{Request, State},
    http::{StatusCode, header::AUTHORIZATION},
    middleware::Next,
    response::Response,
};

use crate::state::AppState;

pub async fn require_bearer(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let presented = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;

    // 1. OAuth access token? sha256 + index lookup; cheap.
    let oauth_ok = match state.oauth().find_access_token(presented).await {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(e) => {
            tracing::error!("oauth access-token lookup failed: {e}");
            false
        }
    };
    if oauth_ok {
        return Ok(next.run(request).await);
    }

    // 2. Static bearer fallback (legacy CLI; goes away when CLI moves to
    //    OAuth in P4).
    if constant_time_eq(presented.as_bytes(), state.bearer_token().as_bytes()) {
        return Ok(next.run(request).await);
    }

    Err(StatusCode::UNAUTHORIZED)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
