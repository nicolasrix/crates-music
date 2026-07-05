//! Tethered-drift autoplay reducer logic — the TUI port of the web's
//! `AutoplayContext`. When autoplay is on and the queue is about to drain,
//! [`maybe_refill`] (driven off the 250 ms tick) gathers the current queue
//! shape and fires one refill effect; [`on_refilled`] pushes the results,
//! records their provenance, and arms the cooldown. Feedback thumbs
//! ([`feedback`]) rate the now-playing autoplay pick.
//!
//! The refill is **poll-driven** rather than reactive: checking a cheap
//! upcoming-count each tick is simpler than hooking every queue-mutation path
//! and, with the in-flight lock + cooldown, fires no more eagerly than the
//! web's per-broadcast effect. Autoplay works both online (pushes over sync)
//! and offline (local enqueue) — it only needs the gateway's HTTP
//! recommender, not the sync WS.

use crate::tui::msg::{Effect, StationError};
use crate::tui::state::{App, FeedbackVote, SyncPhase, to_queued};

use super::playback;

/// Post-refill lock hold (~1.5 s at the 250 ms tick) — outlasts the sync
/// echo so a full delivery can't double-fire.
const COOLDOWN_TICKS: u64 = 6;
/// Under-delivery / error cooldown (~30 s) — an empty or degraded recommender
/// must not busy-loop; the next cursor advance changes the seed pool anyway.
const UNDERDELIVERY_TICKS: u64 = 120;

/// `A` — flip autoplay. Toggling bumps the generation (so an in-flight refill
/// is discarded on completion) and clears the in-flight lock + cooldown so
/// turning it back on refills promptly.
pub(super) fn toggle(app: &mut App) -> Vec<Effect> {
    app.autoplay.enabled = !app.autoplay.enabled;
    app.autoplay.generation += 1;
    app.autoplay.inflight = false;
    app.autoplay.cooldown_until = 0;
    let note = if app.autoplay.enabled {
        "autoplay on — keeping the queue topped up"
    } else {
        "autoplay off"
    };
    app.set_status(note, false);
    // Don't wait a whole tick to start filling an already-short queue.
    if app.autoplay.enabled {
        return maybe_refill(app);
    }
    vec![]
}

/// Per-tick refill check. Cheap enough to run every tick; the guards below
/// make it a near-instant no-op in the common case.
pub(super) fn maybe_refill(app: &mut App) -> Vec<Effect> {
    if !app.autoplay.enabled || app.autoplay.inflight {
        return vec![];
    }
    // Autoplay needs the gateway recommender (HTTP); direct mode has none.
    if app.sync.phase == SyncPhase::Disabled {
        return vec![];
    }
    if app.tick < app.autoplay.cooldown_until {
        return vec![];
    }
    // Need a cursor to measure "upcoming" against (web parity).
    let Some(cursor) = app.queue.current_index() else {
        return vec![];
    };
    let len = app.queue.len();
    let upcoming = len.saturating_sub(cursor + 1);
    let min_upcoming = app.autoplay.cfg.min_upcoming;
    if upcoming >= min_upcoming {
        return vec![];
    }
    let need = min_upcoming - upcoming;

    let queue_track_ids: Vec<String> = app.queue.items().iter().map(|t| t.id.clone()).collect();
    if queue_track_ids.is_empty() {
        return vec![];
    }
    // Sorted for a deterministic effect payload (the set membership the effect
    // rebuilds is order-independent).
    let mut recommended: Vec<String> = app.autoplay.recommended.iter().cloned().collect();
    recommended.sort();

    // Session anchor + id come from the room (online only); offline refills
    // still work, just without the weight-3 anchor seed.
    let (anchor_track_id, session_id) = app.sync.session_anchor().map_or((None, None), |a| {
        (
            Some(a.track_id.as_str().to_owned()),
            Some(a.session_id.as_str().to_owned()),
        )
    });

    app.autoplay.inflight = true;
    vec![Effect::AutoplayRefill {
        queue_track_ids,
        now_playing_index: cursor,
        recommended,
        anchor_track_id,
        session_id,
        need,
        generation: app.autoplay.generation,
    }]
}

/// A refill resolved. Drop it if autoplay was toggled since (generation
/// guard); otherwise push the tracks, record provenance, and arm the cooldown
/// — a shortfall gets the long cooldown so an empty recommender can't hot-loop.
pub(super) fn on_refilled(
    app: &mut App,
    generation: u64,
    need: usize,
    result: Result<Vec<music_core::Track>, StationError>,
) -> Vec<Effect> {
    if generation != app.autoplay.generation {
        // Toggled (or superseded) while in flight — discard. `toggle` already
        // released the lock, so leave it be.
        return vec![];
    }
    app.autoplay.inflight = false;

    let (added, effects) = match result {
        Ok(tracks) if !tracks.is_empty() => {
            let n = tracks.len();
            for t in &tracks {
                app.autoplay.recommended.insert(t.id.as_str().to_owned());
            }
            let queued: Vec<_> = tracks.iter().map(to_queued).collect();
            app.set_status(format!("autoplay added {n} track(s)"), false);
            (n, playback::enqueue_tracks(app, queued, false))
        }
        Ok(_) => {
            // Empty but OK — recommender warming up or nothing new to add.
            (0, vec![])
        }
        Err(StationError::Unavailable) => {
            // Degraded recommender — stay quiet (the long cooldown handles it).
            (0, vec![])
        }
        Err(StationError::Other(e)) => {
            app.set_status(format!("autoplay refill failed: {e}"), true);
            (0, vec![])
        }
    };

    let cooldown = if added < need {
        UNDERDELIVERY_TICKS
    } else {
        COOLDOWN_TICKS
    };
    app.autoplay.cooldown_until = app.tick + cooldown;
    effects
}

/// `f` / `F` — thumbs up/down on the now-playing track. A no-op (with a status
/// line) unless that track was an autoplay pick. Pressing the active thumb
/// again clears the vote (web parity).
pub(super) fn feedback(app: &mut App, vote: FeedbackVote) -> Vec<Effect> {
    let Some(track_id) = app.queue.current().map(|t| t.id.clone()) else {
        app.set_status("nothing playing to rate", false);
        return vec![];
    };
    if !app.autoplay.recommended.contains(&track_id) {
        app.set_status("thumbs only apply to autoplay picks", false);
        return vec![];
    }

    let previous = app.autoplay.votes.get(&track_id).copied();
    // Clicking the active thumb again clears it.
    let target = if previous == Some(vote) { None } else { Some(vote) };
    match target {
        Some(v) => {
            app.autoplay.votes.insert(track_id.clone(), v);
        }
        None => {
            app.autoplay.votes.remove(&track_id);
        }
    }

    let session_id = app.sync.session_anchor().map_or_else(
        || app.autoplay.feedback_session.clone(),
        |a| a.session_id.as_str().to_owned(),
    );

    let note = match target {
        Some(FeedbackVote::Up) => "👍 more like this",
        Some(FeedbackVote::Down) => "👎 less like this",
        None => "cleared feedback",
    };
    app.set_status(note, false);

    vec![Effect::SubmitFeedback {
        track_id,
        vote: target,
        session_id,
        previous,
    }]
}

/// A feedback write finished — roll the optimistic vote back on failure.
pub(super) fn on_feedback_done(
    app: &mut App,
    track_id: String,
    previous: Option<FeedbackVote>,
    result: Result<(), String>,
) -> Vec<Effect> {
    if let Err(e) = result {
        match previous {
            Some(v) => {
                app.autoplay.votes.insert(track_id, v);
            }
            None => {
                app.autoplay.votes.remove(&track_id);
            }
        }
        app.set_status(format!("feedback failed: {e}"), true);
    }
    vec![]
}
