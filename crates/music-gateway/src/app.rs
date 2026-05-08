//! Router construction. Kept as a free function so tests can drive it via
//! `tower::ServiceExt::oneshot` without binding a TCP port.

use axum::{
    Json, Router,
    http::StatusCode,
    middleware::from_fn_with_state,
    routing::{any, get, post},
};
use serde_json::json;
use tower_http::trace::TraceLayer;

use crate::auth::require_bearer;
use crate::oauth::handlers as oauth_handlers;
use crate::proxy::proxy;
use crate::state::AppState;

pub fn build_router(state: AppState) -> Router {
    let public = Router::new()
        .route("/healthz", get(healthz))
        .route("/oauth/setup", post(oauth_handlers::setup))
        .route(
            "/oauth/login",
            get(oauth_handlers::login_get).post(oauth_handlers::login_post),
        )
        .route("/oauth/authorize", get(oauth_handlers::authorize))
        .route("/oauth/token", post(oauth_handlers::token))
        .route("/oauth/revoke", post(oauth_handlers::revoke));

    let protected = Router::new()
        .route("/v1/whoami", get(whoami))
        .route("/rest/*subsonic_path", any(proxy))
        .layer(from_fn_with_state(state.clone(), require_bearer));

    public
        .merge(protected)
        // Explicit 404 fallback. Without this, unmatched routes inherit the
        // protected sub-router's `require_bearer` layer (which wraps its
        // own fallback) and incorrectly return 401.
        .fallback(not_found)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

async fn healthz() -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "service": "music-gateway",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn whoami() -> Json<serde_json::Value> {
    Json(json!({
        "service": "music-gateway",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
