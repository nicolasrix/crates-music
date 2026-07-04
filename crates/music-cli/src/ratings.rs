//! `like` / `dislike` / `unrate` / `liked` — the user's durable taste.
//!
//! Ratings are **gateway-owned**: there is no Navidrome writeback by design
//! (a dislike hard-excludes an entity from play; a like boosts it). These
//! commands are thin printing wrappers over the typed fetchers in
//! [`crate::api`], which the TUI shares.

use anyhow::Result;

use crate::api::{self, RatingItem};
use crate::cli::RatingKind;
use crate::config::Config;

/// `PUT /v1/library/rating` — set (`Some`) or clear (`None`) one entity's
/// verdict. `rating` is the wire string `"like"` / `"dislike"`.
pub async fn run_set(
    config: &Config,
    kind: RatingKind,
    id: &str,
    rating: Option<&str>,
) -> Result<()> {
    api::set_rating(config, kind.wire(), id, rating).await?;

    let verb = match rating {
        Some("like") => "liked",
        Some("dislike") => "disliked",
        _ => "cleared rating on",
    };
    println!("{verb} {} {id}", kind.wire());
    Ok(())
}

/// `GET /v1/library/ratings` — every rated entity, printed in two sections.
/// Dislikes are shown too (they govern exclusion, so the user wants them
/// visible and clearable), but the command is named `liked` to match the
/// web "Liked" surface.
pub async fn run_liked(config: &Config) -> Result<()> {
    let ratings = api::fetch_ratings(config).await?;

    let section = |verdict: &str| -> Vec<&RatingItem> {
        ratings
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
