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
use crate::diagnostics::handlers as diagnostics_handlers;
use crate::events;
use crate::oauth::handlers as oauth_handlers;
use crate::proxy::proxy;
use crate::recommend;
use crate::recommend_feedback;
use crate::scrobble;
use crate::state::AppState;
use crate::sync::handlers as sync_handlers;

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
        .route("/v1/sync/snapshot", get(sync_handlers::snapshot))
        .route("/v1/sync/ops", post(sync_handlers::submit_op))
        .route("/v1/sync", get(crate::sync::ws::ws_handler))
        .route("/v1/recommend/next", get(recommend::next))
        .route("/v1/recommend/from-seeds", post(recommend::from_seeds))
        .route("/v1/recommend/from-any", post(recommend::from_any))
        .route("/v1/recommend/enqueue", post(recommend::enqueue))
        .route("/v1/recommend/feedback", post(recommend_feedback::submit))
        .route("/v1/events", post(events::submit))
        .route("/v1/diagnostics/traces", get(diagnostics_handlers::traces))
        .route(
            "/v1/diagnostics/histogram",
            get(diagnostics_handlers::histogram),
        )
        .route(
            "/v1/diagnostics/queue_depth",
            get(diagnostics_handlers::queue_depth),
        )
        .route(
            "/v1/diagnostics/recently_played",
            get(diagnostics_handlers::recently_played),
        )
        .route(
            "/v1/diagnostics/client_events",
            get(diagnostics_handlers::list_client_events)
                .post(diagnostics_handlers::submit_client_events),
        )
        .route(
            "/v1/diagnostics/recommend/queue_fill",
            get(diagnostics_handlers::recommend_queue_fill),
        )
        .route(
            "/v1/diagnostics/recommend/shortfall",
            get(diagnostics_handlers::recommend_shortfall),
        )
        .route(
            "/v1/diagnostics/recommend/similarity",
            get(diagnostics_handlers::recommend_similarity),
        )
        .route(
            "/v1/diagnostics/recommend/top_results",
            get(diagnostics_handlers::recommend_top_results),
        )
        .route(
            "/v1/diagnostics/recommend/feedback",
            get(diagnostics_handlers::recommend_feedback),
        )
        .route(
            "/v1/diagnostics/recommend/latent_space",
            get(diagnostics_handlers::recommend_latent_space),
        )
        // /rest/scrobble is intercepted to write the recommender's
        // recency clock before delegating to the same proxy used by
        // every other /rest/* call. axum's matchit prefers the more
        // specific path over the wildcard, so this wins regardless of
        // registration order.
        .route("/rest/scrobble", any(scrobble::scrobble))
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
