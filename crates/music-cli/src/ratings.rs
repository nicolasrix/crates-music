//! `like` / `dislike` / `unrate` / `liked` — the user's durable taste.
//!
//! Ratings are **gateway-owned**: there is no Navidrome writeback by design
//! (a dislike hard-excludes an entity from play; a like boosts it). These
//! commands are thin clients over `PUT/GET /v1/library/rating(s)`.

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::cli::RatingKind;
use crate::config::Config;
use crate::gateway::{endpoint, http_client, require_gateway};

/// `PUT /v1/library/rating` — set (`Some`) or clear (`None`) one entity's
/// verdict. `rating` is the wire string `"like"` / `"dislike"`.
pub async fn run_set(
    config: &Config,
    kind: RatingKind,
    id: &str,
    rating: Option<&str>,
) -> Result<()> {
    let gw = require_gateway(config)?;
    let url = endpoint(gw, "/v1/library/rating");
    // `rating: null` is a clear — serde_json renders `None` as JSON null.
    let body = serde_json::json!({ "kind": kind.wire(), "id": id, "rating": rating });

    let resp = http_client(gw)?
        .put(&url)
        .bearer_auth(&gw.bearer_token)
        .json(&body)
        .send()
        .await
        .context("sending rating")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        bail!("rating rejected ({status}): {text}");
    }

    let verb = match rating {
        Some("like") => "liked",
        Some("dislike") => "disliked",
        _ => "cleared rating on",
    };
    println!("{verb} {} {id}", kind.wire());
    Ok(())
}

#[derive(Debug, Deserialize)]
struct RatingItem {
    kind: String,
    id: String,
    /// `"like"` / `"dislike"`; absent/`null` means neutral (the server
    /// doesn't return those, but be defensive).
    rating: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RatingsResponse {
    ratings: Vec<RatingItem>,
}

/// `GET /v1/library/ratings` — every rated entity, printed in two sections.
/// Dislikes are shown too (they govern exclusion, so the user wants them
/// visible and clearable), but the command is named `liked` to match the
/// web "Liked" surface.
pub async fn run_liked(config: &Config) -> Result<()> {
    let gw = require_gateway(config)?;
    let url = endpoint(gw, "/v1/library/ratings");
    let resp: RatingsResponse = http_client(gw)?
        .get(&url)
        .bearer_auth(&gw.bearer_token)
        .send()
        .await
        .context("requesting ratings")?
        .error_for_status()
        .context("ratings returned error status")?
        .json()
        .await
        .context("parsing ratings")?;

    let section = |verdict: &str| -> Vec<&RatingItem> {
        resp.ratings
            .iter()
            .filter(|r| r.rating.as_deref() == Some(verdict))
            .collect()
    };
    let liked = section("like");
    let disliked = section("dislike");

    if liked.is_empty() && disliked.is_empty() {
        println!("(no ratings)");
        return Ok(());
    }
    if !liked.is_empty() {
        println!("LIKED");
        print_items(&liked);
    }
    if !disliked.is_empty() {
        if !liked.is_empty() {
            println!();
        }
        println!("DISLIKED");
        print_items(&disliked);
    }
    Ok(())
}

/// `  <kind>   <id>`, kind column padded to the widest of track/album/artist.
fn print_items(items: &[&RatingItem]) {
    for r in items {
        println!("  {:<6}  {}", r.kind, r.id);
    }
}
