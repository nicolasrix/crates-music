//! `station` and `recommend next` — the gateway's content recommender
//! surfaced on the CLI.
//!
//! The gateway returns ranked `{track_id, similarity}` rows; it does not
//! echo track metadata. So we resolve each id back to a `Track` through the
//! Subsonic client and print the familiar tracks table — the row order is
//! the similarity ranking. HTTP + parsing live in [`crate::api`] (shared
//! with the TUI); this module only renders.

use anyhow::{Result, bail};
use music_subsonic::Client;

use crate::api::{self, ApiError};
use crate::config::Config;
use crate::format::tracks_table;

/// `GET /v1/recommend/station?text=<prompt>&n=<n>` — a natural-language
/// station. Resolves the returned ids to titles and prints them in rank
/// order.
pub async fn run_station(config: &Config, client: &Client, prompt: &str, n: usize) -> Result<()> {
    let list = match api::station(config, prompt, n).await {
        Ok(list) => list,
        Err(ApiError::RecommenderUnavailable) => bail!(
            "station unavailable: the gateway recommender is not ready \
             (embedder unreachable, or no tracks embedded yet)"
        ),
        Err(ApiError::Http(e)) => return Err(e),
    };
    print_resolved(client, &list.track_ids).await;
    Ok(())
}

/// `GET /v1/recommend/next?seed=<id>&n=<n>` — tracks acoustically similar
/// to a seed track. Prints them in rank order; notes degraded mode.
pub async fn run_next(config: &Config, client: &Client, seed: &str, n: usize) -> Result<()> {
    let list = match api::recommend_next(config, seed, n).await {
        Ok(list) => list,
        Err(ApiError::RecommenderUnavailable) => bail!(
            "no recommendations for {seed}: the seed track isn't embedded \
             yet, or the recommender is not ready"
        ),
        Err(ApiError::Http(e)) => return Err(e),
    };

    if list.degraded {
        eprintln!(
            "(degraded mode: tag-only similarity — seed not embedded or \
             recommender warming up)"
        );
    }
    print_resolved(client, &list.track_ids).await;
    Ok(())
}

/// Resolve `ids` to tracks (concurrently, order preserved) and print the
/// tracks table. Ids that fail to resolve are listed afterwards so the
/// ranking the recommender returned is never silently truncated.
async fn print_resolved(client: &Client, ids: &[music_core::TrackId]) {
    if ids.is_empty() {
        println!("(no results)");
        return;
    }

    let (tracks, failed) = api::resolve_tracks(client, ids).await;

    print!("{}", tracks_table(&tracks));
    if !failed.is_empty() {
        eprintln!(
            "(could not resolve {} track(s): {})",
            failed.len(),
            failed.join(", ")
        );
    }
}
