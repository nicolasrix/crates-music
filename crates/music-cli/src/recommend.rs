//! `station` (and, in the next slice, `recommend next`) — the gateway's
//! content recommender surfaced on the CLI.
//!
//! The gateway returns ranked `{track_id, similarity}` rows; it does not
//! echo track metadata. So we resolve each id back to a `Track` through the
//! Subsonic client and print the familiar tracks table — the row order is
//! the similarity ranking.

use anyhow::{Context, Result, bail};
use futures_util::future::join_all;
use music_core::TrackId;
use music_subsonic::Client;
use reqwest::StatusCode;
use serde::Deserialize;

use crate::config::Config;
use crate::format::tracks_table;
use crate::gateway::{endpoint, http_client, require_gateway};

#[derive(Debug, Deserialize)]
struct RecommendItem {
    track_id: String,
}

#[derive(Debug, Deserialize)]
struct StationResponse {
    results: Vec<RecommendItem>,
}

#[derive(Debug, Deserialize)]
struct NextResponse {
    /// True when the recommender fell back to tag-only similarity (seed not
    /// embedded, or the index is still warming up).
    #[serde(default)]
    degraded: bool,
    results: Vec<RecommendItem>,
}

/// `GET /v1/recommend/station?text=<prompt>&n=<n>` — a natural-language
/// station. Resolves the returned ids to titles and prints them in rank
/// order.
pub async fn run_station(config: &Config, client: &Client, prompt: &str, n: usize) -> Result<()> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, "/v1/recommend/station");
    let resp = http_client(gw)?
        .get(&url)
        .query(&[("text", prompt), ("n", &n.to_string())])
        .bearer_auth(&token)
        .send()
        .await
        .context("requesting station")?;

    // A 404 here means the recommender is unavailable (no embedder / degraded
    // boot) rather than a missing route — say so plainly.
    if resp.status() == StatusCode::NOT_FOUND {
        bail!(
            "station unavailable: the gateway recommender is not ready \
             (embedder unreachable, or no tracks embedded yet)"
        );
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        bail!("station request failed ({status}): {text}");
    }
    let station: StationResponse = resp.json().await.context("parsing station response")?;

    let ids: Vec<TrackId> = station
        .results
        .into_iter()
        .map(|r| TrackId::from(r.track_id))
        .collect();
    print_resolved(client, &ids).await;
    Ok(())
}

/// `GET /v1/recommend/next?seed=<id>&n=<n>` — tracks acoustically similar
/// to a seed track. Prints them in rank order; notes degraded mode.
pub async fn run_next(config: &Config, client: &Client, seed: &str, n: usize) -> Result<()> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = endpoint(gw, "/v1/recommend/next");
    let resp = http_client(gw)?
        .get(&url)
        .query(&[("seed", seed), ("n", &n.to_string())])
        .bearer_auth(&token)
        .send()
        .await
        .context("requesting recommendations")?;

    if resp.status() == StatusCode::NOT_FOUND {
        bail!(
            "no recommendations for {seed}: the seed track isn't embedded \
             yet, or the recommender is not ready"
        );
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        bail!("recommend request failed ({status}): {text}");
    }
    let next: NextResponse = resp.json().await.context("parsing recommend response")?;

    if next.degraded {
        eprintln!(
            "(degraded mode: tag-only similarity — seed not embedded or \
             recommender warming up)"
        );
    }
    let ids: Vec<TrackId> = next
        .results
        .into_iter()
        .map(|r| TrackId::from(r.track_id))
        .collect();
    print_resolved(client, &ids).await;
    Ok(())
}

/// Resolve `ids` to tracks (concurrently, order preserved) and print the
/// tracks table. Ids that fail to resolve are listed afterwards so the
/// ranking the recommender returned is never silently truncated.
async fn print_resolved(client: &Client, ids: &[TrackId]) {
    if ids.is_empty() {
        println!("(no results)");
        return;
    }

    let resolved = join_all(ids.iter().map(|id| client.get_song(id))).await;

    let mut tracks = Vec::new();
    let mut failed = Vec::new();
    for (id, result) in ids.iter().zip(resolved) {
        match result {
            Ok(track) => tracks.push(track),
            Err(_) => failed.push(id.as_str()),
        }
    }

    print!("{}", tracks_table(&tracks));
    if !failed.is_empty() {
        eprintln!("(could not resolve {} track(s): {})", failed.len(), failed.join(", "));
    }
}
