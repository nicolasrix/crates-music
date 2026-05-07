//! Router construction. Kept as a free function so tests can drive it via
//! `tower::ServiceExt::oneshot` without binding a TCP port.

use axum::{
    Json, Router,
    middleware::from_fn_with_state,
    routing::{any, get},
};
use serde_json::json;

use crate::auth::require_bearer;
use crate::proxy::proxy;
use crate::state::AppState;

pub fn build_router(state: AppState) -> Router {
    let public = Router::new().route("/healthz", get(healthz));

    let protected = Router::new()
        .route("/v1/whoami", get(whoami))
        .route("/rest/*subsonic_path", any(proxy))
        .layer(from_fn_with_state(state.clone(), require_bearer));

    public.merge(protected).with_state(state)
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
