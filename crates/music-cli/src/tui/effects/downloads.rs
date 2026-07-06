//! Offline-download effects: the Downloads section's load + the pin / unpin /
//! bulk-download / warm / evict operations. Kept out of `effects.rs` so that
//! file stays under the size cap. All of it is local `AudioCache` work plus a
//! best-effort hydrate/fetch through the Subsonic client — the same primitives
//! the classic `pin`/`pinned`/`cache` subcommands use.

use std::collections::HashMap;
use std::fmt::Write as _;

use music_cache::{AudioCacheStats, AudioEntry, AudioKey, PinOutcome};
use music_core::TrackId;
use music_subsonic::Client;

use crate::api;
use crate::tui::msg::Msg;
use crate::tui::state::PinnedRow;
use crate::tui::widgets::human_bytes;

use super::Ctx;

fn pin_done(note: String, is_error: bool) -> Msg {
    Msg::PinDone { note, is_error }
}

/// Cache totals + the pinned table. `stats`/`list_pinned` are local SQLite;
/// only [`hydrate_pinned`] touches the network, so an offline client still
/// gets its rows (by id + size).
pub(super) async fn load(
    ctx: &Ctx,
) -> (Result<AudioCacheStats, String>, Result<Vec<PinnedRow>, String>) {
    let stats = ctx.cache.stats().await.map_err(|e| e.to_string());
    let pinned = match ctx.cache.list_pinned().await {
        Ok(entries) => Ok(hydrate_pinned(ctx, entries).await),
        Err(e) => Err(e.to_string()),
    };
    (stats, pinned)
}

/// Resolve pinned entries' track metadata (title/artist) best-effort. A down
/// server (or an unresolvable id) leaves `track: None`; the row falls back to
/// its id and still plays from the local blob.
async fn hydrate_pinned(ctx: &Ctx, entries: Vec<AudioEntry>) -> Vec<PinnedRow> {
    let ids: Vec<TrackId> = entries
        .iter()
        .map(|e| TrackId::from(e.key.track_id.clone()))
        .collect();
    let mut by_id: HashMap<String, music_core::Track> = match ctx.subsonic().await {
        Ok(client) => {
            let (tracks, _failed) = api::resolve_tracks(&client, &ids).await;
            tracks
                .into_iter()
                .map(|t| (t.id.as_str().to_owned(), t))
                .collect()
        }
        Err(e) => {
            tracing::debug!(error = %e, "pinned hydration offline — showing ids");
            HashMap::new()
        }
    };
    entries
        .into_iter()
        .map(|e| PinnedRow {
            // `remove` moves the Track out — ids are unique per entry, and the
            // map is discarded right after, so there's nothing to clone for.
            track: by_id.remove(&e.key.track_id),
            track_id: e.key.track_id,
            bytes: e.bytes,
        })
        .collect()
}

/// Fetch a track's bytes into the cache if they aren't there yet (the
/// pin-if-missing precondition). Idempotent: a hit is a no-op. Takes an
/// already-resolved client so a bulk caller resolves auth once, not per track.
async fn ensure_cached(
    ctx: &Ctx,
    client: &Client,
    tid: &TrackId,
    key: &AudioKey,
) -> anyhow::Result<()> {
    if ctx.cache.get(key).await?.is_some() {
        return Ok(());
    }
    crate::app::fetch_track_bytes(client, &ctx.cache, tid, ctx.download_quality()).await?;
    Ok(())
}

/// `d` — toggle one track's offline pin. Already pinned (at *any* quality) →
/// unpin that entry; otherwise fetch-if-missing then pin at the current
/// download quality. Mirrors the classic `pin`/`unpin` outcomes.
pub(super) async fn pin_toggle(ctx: &Ctx, track_id: &str, title: &str) -> Msg {
    let tid = TrackId::from(track_id.to_owned());

    // Is this track pinned under *any* quality? If so, `d` un-saves that exact
    // entry — keying off today's `download_quality` would miss a copy pinned
    // before the setting changed (and silently pin a second one).
    match ctx.cache.find_by_track(track_id).await {
        Ok(Some(entry)) if entry.pinned => {
            return match ctx.cache.unpin(&entry.key).await {
                Ok(_) => pin_done(format!("removed {title} from offline"), false),
                Err(e) => pin_done(format!("unpin failed: {e}"), true),
            };
        }
        Ok(_) => {}
        Err(e) => return pin_done(format!("cache error: {e}"), true),
    }

    let key = crate::app::audio_key(&tid, ctx.download_quality());
    let client = match ctx.subsonic().await {
        Ok(c) => c,
        Err(e) => return pin_done(format!("save failed: {e}"), true),
    };
    if let Err(e) = ensure_cached(ctx, &client, &tid, &key).await {
        return pin_done(format!("save failed: {e}"), true);
    }
    match ctx.cache.pin(&key).await {
        Ok(PinOutcome::Pinned) => pin_done(format!("saved {title} for offline"), false),
        Ok(PinOutcome::AlreadyPinned) => pin_done(format!("{title} already offline"), false),
        Ok(PinOutcome::NotInCache) => {
            pin_done(format!("save failed: {title} vanished from cache"), true)
        }
        Ok(PinOutcome::WouldExceedBudget { over_by }) => pin_done(
            format!("pinned budget full — over by {}", human_bytes(over_by)),
            true,
        ),
        Err(e) => pin_done(format!("save failed: {e}"), true),
    }
}

/// `W` on a collection — pin every track (fetch-if-missing), tolerant per
/// track. One status line tallies saved / already-offline / failed.
pub(super) async fn pin_bulk(ctx: &Ctx, track_ids: &[String], label: &str) -> Msg {
    // Resolve the client once — a fresh album is all cache-misses, and
    // re-resolving auth per track would relock + reread the token store N
    // times on the heaviest gesture there is.
    let client = match ctx.subsonic().await {
        Ok(c) => c,
        Err(e) => return pin_done(format!("{label} failed: {e}"), true),
    };
    let quality = ctx.download_quality();
    let (mut saved, mut already, mut failed) = (0usize, 0usize, 0usize);
    let mut budget_hit = false;
    for id in track_ids {
        let tid = TrackId::from(id.clone());
        let key = crate::app::audio_key(&tid, quality);
        if ensure_cached(ctx, &client, &tid, &key).await.is_err() {
            failed += 1;
            continue;
        }
        match ctx.cache.pin(&key).await {
            Ok(PinOutcome::Pinned) => saved += 1,
            Ok(PinOutcome::AlreadyPinned) => already += 1,
            Ok(PinOutcome::WouldExceedBudget { .. }) => {
                budget_hit = true;
                failed += 1;
            }
            Ok(PinOutcome::NotInCache) | Err(_) => failed += 1,
        }
    }
    let mut note = format!("{label}: {saved} saved");
    if already > 0 {
        let _ = write!(note, ", {already} already offline");
    }
    if failed > 0 {
        let _ = write!(note, ", {failed} failed");
    }
    if budget_hit {
        note.push_str(" (pinned budget full)");
    }
    pin_done(note, failed > 0)
}

/// Downloads `W` — warm the cache from every liked *track*.
pub(super) async fn warm_liked(ctx: &Ctx) -> Msg {
    let ratings = match api::fetch_ratings(&ctx.config).await {
        Ok(r) => r,
        Err(e) => return pin_done(format!("warm failed: {e}"), true),
    };
    let ids: Vec<String> = ratings
        .into_iter()
        .filter(|r| r.kind == "track" && r.rating.as_deref() == Some("like"))
        .map(|r| r.id)
        .collect();
    if ids.is_empty() {
        return pin_done("no liked tracks to warm".to_owned(), false);
    }
    pin_bulk(ctx, &ids, "warm from liked").await
}

/// `E` — fit the regular cache to its budget now.
pub(super) async fn evict(ctx: &Ctx) -> Msg {
    match ctx.cache.evict_lru_to_fit().await {
        Ok(total) => pin_done(
            format!("evicted — regular cache now {}", human_bytes(total)),
            false,
        ),
        Err(e) => pin_done(format!("evict failed: {e}"), true),
    }
}
