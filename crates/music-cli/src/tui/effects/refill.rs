//! The autoplay refill effect: turn the reducer's Eq-friendly seed *inputs*
//! into a `from-seeds` (or `from-any` fallback) recommend call and resolve the
//! ids to tracks. Kept out of `effects.rs` so that file stays under the
//! size cap; the float weighting lives here (via the pure builder + config)
//! rather than in the `Effect`.

use std::collections::HashSet;

use crate::api::{self, WeightedStation};
use crate::tui::autoplay::{Frontier, build_seeds};
use crate::tui::msg::StationError;

use super::{Ctx, resolve_list, station_error};

/// Gateway request caps (`crates/music-gateway/src/recommend.rs`): a long
/// session's queue can exceed these, and the gateway 400s rather than trims —
/// so cap client-side. Seeds are weight-sorted, the queue is tail-capped
/// (nearest the cursor), so the caps drop the least-relevant entries.
const MAX_SEEDS: usize = 200;
const MAX_QUEUE: usize = 400;

/// Build the weighted seeds (pure builder + `[tui.autoplay]` drift config) and
/// run one refill: `from-seeds` when there are weighted seeds, else `from-any`
/// over the reversed queue (newest-first). Both paths attach a `queue_context`
/// so the gateway excludes already-queued tracks. An unindexed/degraded
/// recommender surfaces as an empty `Ok` or `StationError::Unavailable`, which
/// the reducer reads as under-delivery.
pub(super) async fn run(
    ctx: &Ctx,
    queue_track_ids: &[String],
    now_playing_index: usize,
    recommended: &HashSet<String>,
    anchor_track_id: Option<&str>,
    session_id: &str,
    need: usize,
) -> Result<Vec<music_core::Track>, StationError> {
    let ap = &ctx.config.tui.autoplay;
    let mut seeds = build_seeds(
        queue_track_ids,
        now_playing_index,
        recommended,
        anchor_track_id,
        Frontier::from(ap),
    );
    seeds.seeds.truncate(MAX_SEEDS);
    seeds.anchor_ids.truncate(MAX_SEEDS);

    // Tail-cap the queue context (keeps the cursor + upcoming, which matter
    // most for dedup/diversity).
    let queue_ctx: &[String] = if queue_track_ids.len() > MAX_QUEUE {
        &queue_track_ids[queue_track_ids.len() - MAX_QUEUE..]
    } else {
        queue_track_ids
    };
    let now_playing = queue_track_ids.get(now_playing_index).map(String::as_str);

    let list = if seeds.seeds.is_empty() {
        // No user / anchor / frontier seeds — fall back to from-any over the
        // queue, newest first (mirrors the web fallback), with queue dedup.
        let candidates: Vec<String> = queue_track_ids.iter().rev().take(MAX_SEEDS).cloned().collect();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        api::recommend_from_any(&ctx.config, &candidates, need, queue_ctx, now_playing).await
    } else {
        let req = WeightedStation {
            seeds: &seeds.seeds,
            anchor_track_ids: &seeds.anchor_ids,
            queue_track_ids: queue_ctx,
            now_playing_track_id: now_playing,
            session_id: Some(session_id),
            n: need,
            leash_tau: ap.leash_tau,
            leash_lambda: ap.leash_lambda,
            mmr_lambda: ap.mmr_lambda,
        };
        api::recommend_weighted_station(&ctx.config, &req).await
    };

    match list {
        Ok(list) => resolve_list(ctx, &list.track_ids).await,
        Err(e) => Err(station_error(e)),
    }
}
