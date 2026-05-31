//! Router construction. Kept as a free function so tests can drive it via
//! `tower::ServiceExt::oneshot` without binding a TCP port.

use axum::{
    Json, Router,
    extract::DefaultBodyLimit,
    http::StatusCode,
    middleware::from_fn_with_state,
    routing::{any, get, post},
};
use serde_json::json;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::admin;
use crate::auth::require_bearer;
use crate::diagnostics::handlers as diagnostics_handlers;
use crate::events;
use crate::oauth::handlers as oauth_handlers;
use crate::proxy::proxy;
use crate::readyz;
use crate::recommend;
use crate::recommend_feedback;
use crate::scrobble;
use crate::state::AppState;
use crate::sync::handlers as sync_handlers;

/// Aggregate request-body cap for the JSON `/v1/*` API.
///
/// This is a coarse memory-abuse backstop that fires *before* a handler
/// buffers and parses the body — distinct from the per-field/per-batch
/// caps inside the handlers (those bound individual fields once parsed).
/// 1 MiB comfortably covers the largest realistic request (a full
/// `MAX_BATCH` scrobble batch with small per-event metadata, or a
/// 1000-id enqueue ≈ 45 KiB) while rejecting multi-MB payloads. It is
/// deliberately tighter than axum's 2 MiB default to make intent
/// explicit. A client that wants to send 1000 events *each* near the
/// 4 KiB metadata cap must split across requests — the aggregate cap is
/// orthogonal to the per-field cap by design.
///
/// NOT applied to `/rest/*`: the Subsonic proxy and audio-stream paths
/// forward bodies for Navidrome and must not inherit this limit.
const MAX_V1_BODY_BYTES: usize = 1024 * 1024;

pub fn build_router(state: AppState) -> Router {
    let public = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz::readyz))
        .route("/oauth/setup", post(oauth_handlers::setup))
        .route(
            "/oauth/login",
            get(oauth_handlers::login_get).post(oauth_handlers::login_post),
        )
        .route("/oauth/authorize", get(oauth_handlers::authorize))
        .route("/oauth/token", post(oauth_handlers::token))
        .route("/oauth/revoke", post(oauth_handlers::revoke));

    let v1 = Router::new()
        .route("/v1/whoami", get(whoami))
        .route("/v1/sync/snapshot", get(sync_handlers::snapshot))
        .route("/v1/sync/ops", post(sync_handlers::submit_op))
        .route("/v1/sync", get(crate::sync::ws::ws_handler))
        .route("/v1/recommend/next", get(recommend::next))
        .route("/v1/recommend/station", get(recommend::station))
        .route("/v1/recommend/from-seeds", post(recommend::from_seeds))
        .route("/v1/recommend/from-any", post(recommend::from_any))
        .route("/v1/recommend/similar_albums", post(recommend::similar_albums))
        .route("/v1/recommend/similar_artists", post(recommend::similar_artists))
        .route("/v1/recommend/enqueue", post(recommend::enqueue))
        .route(
            "/v1/recommend/refit_whitening",
            post(recommend::refit_whitening),
        )
        .route("/v1/recommend/feedback", post(recommend_feedback::submit))
        .route("/v1/events", post(events::submit))
        .route(
            "/v1/admin/cache/invalidate",
            post(admin::invalidate_cache),
        )
        .route(
            "/v1/admin/cache/invalidate_covers",
            post(admin::invalidate_covers),
        )
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
            "/v1/diagnostics/span_series",
            get(diagnostics_handlers::span_series),
        )
        .route(
            "/v1/diagnostics/span_children",
            get(diagnostics_handlers::span_children),
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
        .route(
            "/v1/diagnostics/recommend/latent_neighbours",
            get(diagnostics_handlers::recommend_latent_neighbours),
        )
        .route(
            "/v1/diagnostics/recommend/sessions",
            get(diagnostics_handlers::recommend_sessions),
        )
        // Coarse body cap on the JSON API only — see MAX_V1_BODY_BYTES.
        // Scoped to this sub-router so it does NOT reach the /rest proxy
        // below once the two are merged.
        .layer(DefaultBodyLimit::max(MAX_V1_BODY_BYTES));

    let rest = Router::new()
        // /rest/scrobble is intercepted to write the recommender's
        // recency clock before delegating to the same proxy used by
        // every other /rest/* call. axum's matchit prefers the more
        // specific path over the wildcard, so this wins regardless of
        // registration order.
        .route("/rest/scrobble", any(scrobble::scrobble))
        .route("/rest/*subsonic_path", any(proxy));

    let protected = v1
        .merge(rest)
        .layer(from_fn_with_state(state.clone(), require_bearer));

    let merged = public.merge(protected);

    // When `server.static_dir` is configured, the gateway hosts the web
    // SPA from `<static_dir>/`:
    //   * `/assets/*` is served by a non-fallback `ServeDir` so that a
    //     missing hashed bundle is a real 404 (catches deploy skew).
    //   * Everything else that didn't match an API route falls back to
    //     `index.html` so React Router can take over client-side.
    // When `static_dir` is `None`, the legacy explicit 404 fallback
    // applies — preserving the no-SPA behaviour for dev where Vite
    // serves the bundle on its own port.
    let with_fallback = match state.config().server.static_dir.as_ref() {
        Some(dir) => {
            let index = dir.join("index.html");
            let assets_dir = dir.join("assets");
            merged
                .nest_service("/assets", ServeDir::new(assets_dir))
                .fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)))
        }
        None => merged.fallback(not_found),
    };

    with_fallback
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
