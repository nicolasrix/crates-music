//! Operator endpoints. Today: just the cache invalidator.
//!
//! `POST /v1/admin/cache/invalidate` clears every non-cover-art row
//! from the L2 browse cache. Use case: new content added in Navidrome
//! is hidden by a fresh cache entry for up to `browse_ttl_seconds`
//! (default 24 h). This endpoint forces an upstream refetch on the
//! next browse request without restarting the gateway.
//!
//! Cover-art rows are preserved. Navidrome's `coverArt` ids are
//! content-addressed: when a cover changes, the id changes, and the
//! old entry becomes unreachable rather than stale.

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
