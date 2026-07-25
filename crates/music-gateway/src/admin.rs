//! Operator endpoints for cache management.
//!
//! `POST /v1/admin/cache/invalidate` — clears browse-cache rows
//! (`getAlbumList2`, `getAlbum`, etc.). Cover-art rows are preserved.
//!
//! `POST /v1/admin/cache/invalidate_covers` — clears cover-art rows.
//! Use when cover art is stuck on stale placeholders despite the
//! background revalidation mechanism.

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Serialize;

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
