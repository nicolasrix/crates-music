//! Operator endpoints for cache and discovery management.
//!
//! `POST /v1/admin/cache/invalidate` — clears browse-cache rows
//! (`getAlbumList2`, `getAlbum`, etc.). Cover-art rows are preserved.
//!
//! `POST /v1/admin/cache/invalidate_covers` — clears cover-art rows.
//! Use when cover art is stuck on stale placeholders despite the
//! background revalidation mechanism.
//!
//! `POST /v1/admin/discovery/scan` — runs a full catalog sweep now
//! instead of waiting for the timer.

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Serialize;

use crate::discovery::CatalogWatcher;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct InvalidateResponse {
    pub removed: u64,
}

#[tracing::instrument(name = "admin.cache.invalidate", skip_all)]
pub async fn invalidate_cache(State(state): State<AppState>) -> impl IntoResponse {
    match state.cache().clear_browse().await {
        Ok(removed) => {
            tracing::info!(removed, "browse cache invalidated");
            (StatusCode::OK, Json(InvalidateResponse { removed })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "cache invalidate failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[tracing::instrument(name = "admin.cache.invalidate_covers", skip_all)]
pub async fn invalidate_covers(State(state): State<AppState>) -> impl IntoResponse {
    if let Ok(mut set) = state.placeholder_etags().write() {
        set.clear();
    }
    match state.cache().clear_covers().await {
        Ok(removed) => {
            tracing::info!(removed, "cover-art cache invalidated");
            (StatusCode::OK, Json(InvalidateResponse { removed })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "cover-art cache invalidate failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Run a full catalog sweep synchronously and report what it queued.
///
/// The background watcher already does this on a timer; this is the
/// "don't wait" button — after a bulk import, or after re-pointing the
/// gateway at a different Navidrome. It deliberately ignores
/// `[discovery] enabled`, so an operator who runs discovery manually
/// (`enabled = false`) still has a supported way to do it, and it
/// replaces `scripts/enqueue_all_tracks.py` for that use.
///
/// Overlapping with an in-flight scheduled scan is harmless: enqueueing
/// is `INSERT OR IGNORE`, so the duplicate ids are no-ops.
#[tracing::instrument(name = "admin.discovery.scan", skip_all)]
pub async fn discovery_scan(State(state): State<AppState>) -> impl IntoResponse {
    // Same guard as the scheduled watch: a sweep under the unknown-model
    // sentinel would enqueue the entire catalog into a bucket nothing reads.
    if !state.recommend_writes_enabled() {
        tracing::warn!("discovery: manual sweep refused — model_version unknown");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "embedder unreachable at boot; model_version unknown — retry once it is up",
        )
            .into_response();
    }
    let watcher = match CatalogWatcher::new(
        &state.config().upstream,
        state.embedding_store().clone(),
        state.recommend_model_version().clone(),
        state.config().discovery.recent_albums,
    ) {
        Ok(built) => built,
        Err(e) => {
            tracing::error!(error = %e, "discovery: building watcher failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    match watcher.scan_full().await {
        Ok(summary) => {
            tracing::info!(
                seen = summary.seen,
                enqueued = summary.enqueued,
                "discovery: manual full sweep complete"
            );
            (StatusCode::OK, Json(summary)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "discovery: manual full sweep failed");
            // The upstream catalog was unreadable — that's a dependency
            // failure, not a bad request, and it's retryable.
            (StatusCode::BAD_GATEWAY, format!("catalog scan failed: {e}")).into_response()
        }
    }
}
