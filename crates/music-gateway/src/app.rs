//! Router construction. Kept as a free function so tests can drive it via
//! `tower::ServiceExt::oneshot` without binding a TCP port.

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    middleware::from_fn_with_state,
    routing::{any, get, post, put},
};
use serde_json::json;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::admin;
use crate::auth::require_bearer;
use crate::diagnostics::handlers as diagnostics_handlers;
use crate::events;
use crate::guest_codes;
use crate::library_rating;
use crate::oauth::handlers as oauth_handlers;
use crate::playlists::handlers as playlist_handlers;
use crate::proxy::proxy;
use crate::readyz;
use crate::recommend;
use crate::recommend_feedback;
use crate::search::handlers as search_handlers;
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

// A flat, declarative route table — the `too_many_lines` lint fires once
// the list crosses 100 entries' worth of lines, but splitting a route
// registry across helpers hurts readability more than it helps.
#[allow(clippy::too_many_lines)]
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
        .route("/oauth/revoke", post(oauth_handlers::revoke))
        .route("/oauth/logout", post(oauth_handlers::logout))
        .route(
            "/oauth/device_authorization",
            post(oauth_handlers::device_authorization),
        )
        .route(
            "/oauth/device",
            get(oauth_handlers::device_verify_get).post(oauth_handlers::device_verify_post),
        )
        // Guest-code redemption (PR D). Public, like the other grants — the
        // code is the credential.
        .route("/oauth/guest", post(oauth_handlers::guest_grant));

    // Admin-only tier: maintenance + introspection. Guarded by
    // `require_admin`, which reads the `Principal` injected upstream by
    // `require_bearer` and 403s a non-admin. A User or Guest token
    // authenticates (passes `require_bearer`) but is rejected here.
    let v1_admin = Router::new()
        .route("/v1/recommend/enqueue", post(recommend::enqueue))
        .route(
            "/v1/recommend/refit_whitening",
            post(recommend::refit_whitening),
        )
        .route(
            "/v1/admin/cache/invalidate",
            post(admin::invalidate_cache),
        )
        .route(
            "/v1/admin/cache/invalidate_covers",
            post(admin::invalidate_covers),
        )
        .route("/v1/admin/discovery/scan", post(admin::discovery_scan))
        .route(
            "/v1/admin/users",
            get(crate::users::list_users).post(crate::users::create_user),
        )
        .route("/v1/admin/users/:id", axum::routing::delete(crate::users::delete_user))
        .route(
            "/v1/admin/users/:id/password",
            post(crate::users::reset_password),
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
        .route(
            "/v1/diagnostics/recommendations",
            get(diagnostics_handlers::recommendations),
        )
        .layer(axum::middleware::from_fn(crate::principal::require_admin));

    // Any-authenticated tier: browse, play, recommend reads, room
    // control, ratings/events, whoami.
    let v1_general = Router::new()
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
        .route("/v1/recommend/feedback", post(recommend_feedback::submit))
        .route("/v1/library/rating", put(library_rating::put_rating))
        .route("/v1/library/ratings", get(library_rating::list_ratings))
        // Typo-tolerant catalog search (fuzzy fst index; falls back to
        // Navidrome search3 until the index is built).
        .route("/v1/search", get(search_handlers::search))
        .route("/v1/events", post(events::submit))
        // Host-side guest-code management (PR D). Any authenticated real
        // account manages its *own* codes; the handlers 403 a guest.
        .route(
            "/v1/guest_codes",
            get(guest_codes::list).post(guest_codes::create),
        )
        .route("/v1/guest_codes/:id", axum::routing::delete(guest_codes::revoke))
        // Gateway-owned playlists (PR F). Reads are any-authenticated
        // (own + shared); the mutating verbs self-gate on the
        // `WritePlaylist` capability, so a guest token authenticates but
        // gets 403 here rather than a route-level 404.
        .route(
            "/v1/playlists",
            get(playlist_handlers::list).post(playlist_handlers::create),
        )
        .route(
            "/v1/playlists/:id",
            get(playlist_handlers::get)
                .patch(playlist_handlers::patch)
                .delete(playlist_handlers::delete),
        )
        .route(
            "/v1/playlists/:id/tracks",
            put(playlist_handlers::put_tracks),
        );

    let v1 = v1_general
        .merge(v1_admin)
        // Coarse body cap on the JSON API only — see MAX_V1_BODY_BYTES.
        // Scoped here so it does NOT reach the /rest proxy once merged.
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
        .layer(TraceLayer::new_for_http().make_span_with(http_trace_span))
        .with_state(state)
}

/// Span for an inbound HTTP request, replacing tower-http's
/// `DefaultMakeSpan`.
///
/// Records the request **path only** — never the full URI. Access
/// tokens can ride in `?access_token=` (RFC 6750 §2.3, needed for
/// `<audio>`/`<img>` URLs that can't set an `Authorization` header), and
/// the default span records the whole URI, query string included. That
/// value flows into the diagnostics trace store (the layer in
/// `diagnostics` persists every span's fields), so the full URI would
/// leak bearer tokens into SQLite. Stripping to the path closes that.
///
/// Kept at DEBUG to match tower-http's default span level, so trace-store
/// volume is unchanged at the default `info` filter.
pub fn http_trace_span(request: &axum::http::Request<axum::body::Body>) -> tracing::Span {
    tracing::debug_span!(
        "request",
        method = %request.method(),
        path = %request.uri().path(),
        version = ?request.version(),
    )
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

/// Identity of the calling principal, for clients to drive role-gated UI.
///
/// Returns the resolved `Principal` plus the display fields from `users`.
/// Replaces the former static `{service, version}` stub.
async fn whoami(
    State(state): State<AppState>,
    crate::principal::AuthPrincipal(principal): crate::principal::AuthPrincipal,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let (username, display_name) = state
        .oauth()
        .user_profile(principal.user_id)
        .await
        .map_err(|e| {
            tracing::error!("whoami user_profile lookup failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .unwrap_or((None, None));
    Ok(Json(json!({
        "user_id": principal.user_id,
        "role": principal.role,
        "username": username,
        "display_name": display_name,
        "host_user_id": principal.host_user_id,
    })))
}
