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

use std::collections::HashSet;

use music_core::Track;

use crate::tui::msg::{Effect, StationError};
use crate::tui::state::{App, FeedbackVote, to_queued};

use super::playback;

/// Post-refill lock hold (~1.5 s at the 250 ms tick) — outlasts the sync
/// echo so a full delivery can't double-fire.
const COOLDOWN_TICKS: u64 = 6;
/// Under-delivery / error cooldown (~30 s) — an empty or degraded recommender
/// must not busy-loop; the next cursor advance changes the seed pool anyway.
const UNDERDELIVERY_TICKS: u64 = 120;

/// `A` — flip autoplay. Toggling bumps the generation (so an in-flight refill
/// is discarded on completion) and clears the in-flight lock, cooldown, and
/// pending-push set so turning it back on refills promptly from a clean slate.
pub(super) fn toggle(app: &mut App) -> Vec<Effect> {
    app.autoplay.enabled = !app.autoplay.enabled;
    app.autoplay.generation += 1;
    app.autoplay.inflight = false;
    app.autoplay.cooldown_until = 0;
    app.autoplay.pending_push.clear();
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

/// The session id autoplay scopes its recommend + feedback under: the room's
/// session anchor when a session exists, else the per-process fallback. Both
/// paths must agree so a downvote and the refill that could exclude it share
/// one `(track_id, session_id)` bucket.
fn session_id(app: &App) -> String {
    app.sync.session_anchor().map_or_else(
        || app.autoplay.feedback_session.clone(),
        |a| a.session_id.as_str().to_owned(),
    )
}

/// Tracks strictly after the cursor (what "upcoming" measures). A drained
/// queue (no cursor) counts as fully short.
fn upcoming_after_cursor(app: &App) -> usize {
    match app.queue.current_index() {
        Some(c) => app.queue.len().saturating_sub(c + 1),
        None => 0,
    }
}

/// Forget provenance for tracks no longer in the queue: bounds growth and,
/// more importantly, lets a track the user *re-queues by hand* after autoplay
/// dropped it count as a real pick again. Also drop pending-push entries the
/// sync echo has since projected.
fn reconcile(app: &mut App) {
    if app.autoplay.recommended.is_empty() && app.autoplay.pending_push.is_empty() {
        return;
    }
    let in_queue: HashSet<&str> = app.queue.items().iter().map(|t| t.id.as_str()).collect();
    app.autoplay.recommended.retain(|id| in_queue.contains(id.as_str()));
    app.autoplay.pending_push.retain(|id| !in_queue.contains(id.as_str()));
}

/// Per-tick refill check. Cheap enough to run every tick; the guards below
/// make it a near-instant no-op in the common case.
pub(super) fn maybe_refill(app: &mut App) -> Vec<Effect> {
    if !app.autoplay.enabled || app.autoplay.inflight {
        return vec![];
    }
    // Autoplay needs the gateway recommender (HTTP); direct mode has none.
    if app.sync.phase == crate::tui::state::SyncPhase::Disabled {
        return vec![];
    }
    if app.tick < app.autoplay.cooldown_until {
        return vec![];
    }
    // Need a cursor to measure "upcoming" against (web parity).
    let Some(cursor) = app.queue.current_index() else {
        return vec![];
    };

    reconcile(app);
    // Pushed-but-not-yet-echoed tracks count as upcoming, so a slow sync echo
    // can't trigger a second refill for tracks already on their way.
    let upcoming = upcoming_after_cursor(app) + app.autoplay.pending_push.len();
    let min_upcoming = app.autoplay.min_upcoming;
    if upcoming >= min_upcoming {
        return vec![];
    }
    let need = min_upcoming - upcoming;

    let queue_track_ids: Vec<String> = app.queue.items().iter().map(|t| t.id.clone()).collect();
    if queue_track_ids.is_empty() {
        return vec![];
    }
    let anchor_track_id = app
        .sync
        .session_anchor()
        .map(|a| a.track_id.as_str().to_owned());

    app.autoplay.inflight = true;
    vec![Effect::AutoplayRefill {
        queue_track_ids,
        now_playing_index: cursor,
        recommended: app.autoplay.recommended.clone(),
        anchor_track_id,
        session_id: session_id(app),
        need,
        generation: app.autoplay.generation,
    }]
}

/// A refill resolved. Drop it if autoplay was toggled since (generation
/// guard); otherwise dedup the results against the live queue, cap them to the
/// *current* shortfall (the user may have curated the queue mid-flight), push,
/// record provenance, and arm the cooldown.
pub(super) fn on_refilled(
    app: &mut App,
    generation: u64,
    need: usize,
    result: Result<Vec<Track>, StationError>,
) -> Vec<Effect> {
    if generation != app.autoplay.generation {
        // Toggled (or superseded) while in flight — discard. `toggle` already
        // released the lock, so leave it be.
        return vec![];
    }
    app.autoplay.inflight = false;

    let tracks = match result {
        Ok(tracks) => tracks,
        Err(StationError::Unavailable) => {
            // Degraded recommender — stay quiet (the long cooldown handles it).
            arm_cooldown(app, 0, need);
            return vec![];
        }
        Err(StationError::Other(e)) => {
            app.set_status(format!("autoplay refill failed: {e}"), true);
            arm_cooldown(app, 0, need);
            return vec![];
        }
    };

    // Never re-add a track already in the queue (the sync-echo window and the
    // from-any fallback can both surface one), never a dup within the batch,
    // and never exceed the shortfall as it stands *now*.
    let in_queue: HashSet<String> = app.queue.items().iter().map(|t| t.id.clone()).collect();
    let live_need = app.autoplay.min_upcoming.saturating_sub(upcoming_after_cursor(app));
    let mut seen: HashSet<String> = HashSet::new();
    let fresh: Vec<Track> = tracks
        .into_iter()
        .filter(|t| {
            let id = t.id.as_str();
            !in_queue.contains(id) && seen.insert(id.to_owned())
        })
        .take(live_need)
        .collect();

    let added = fresh.len();
    let effects = if added > 0 {
        for t in &fresh {
            app.autoplay.recommended.insert(t.id.as_str().to_owned());
            // Online pushes land only when the echo projects; track them so
            // the next tick doesn't refill for tracks already on the wire.
            if app.sync.online() {
                app.autoplay.pending_push.insert(t.id.as_str().to_owned());
            }
        }
        app.set_status(format!("autoplay added {added} track(s)"), false);
        let queued: Vec<_> = fresh.iter().map(to_queued).collect();
        playback::enqueue_tracks(app, queued, false)
    } else {
        vec![]
    };

    // Cooldown keys on the original request: a genuine shortfall (recommender
    // returned fewer than asked) backs off long; a full delivery pauses ~1.5 s.
    arm_cooldown(app, added, need);
    effects
}

fn arm_cooldown(app: &mut App, added: usize, need: usize) {
    let cooldown = if added < need {
        UNDERDELIVERY_TICKS
    } else {
        COOLDOWN_TICKS
    };
    app.autoplay.cooldown_until = app.tick + cooldown;
}

/// `f` / `F` — thumbs up/down on the now-playing track. A no-op (with a status
/// line) unless that track was an autoplay pick, or a write for it is already
/// in flight. Pressing the active thumb again clears the vote (web parity).
pub(super) fn feedback(app: &mut App, vote: FeedbackVote) -> Vec<Effect> {
    let Some(track_id) = app.queue.current().map(|t| t.id.clone()) else {
        app.set_status("nothing playing to rate", false);
        return vec![];
    };
    if !app.autoplay.recommended.contains(&track_id) {
        app.set_status("thumbs only apply to autoplay picks", false);
        return vec![];
    }
    // Serialize per track (the web's `pending` guard): a second thumb waits for
    // the first to land, so an optimistic rollback can't clobber a newer vote.
    if app.autoplay.feedback_inflight.contains(&track_id) {
        app.set_status("feedback still saving — try again", false);
        return vec![];
    }

    let previous = app.autoplay.votes.get(&track_id).copied();
    // Clicking the active thumb again clears it.
    let target = if previous == Some(vote) { None } else { Some(vote) };
    set_vote(app, &track_id, target);
    app.autoplay.feedback_inflight.insert(track_id.clone());

    let note = match target {
        Some(FeedbackVote::Up) => "👍 more like this",
        Some(FeedbackVote::Down) => "👎 less like this",
        None => "cleared feedback",
    };
    app.set_status(note, false);

    vec![Effect::SubmitFeedback {
        track_id,
        vote: target,
        session_id: session_id(app),
        previous,
    }]
}

/// A feedback write finished — release the per-track lock and, on failure, roll
/// the optimistic vote back (safe: the lock kept any newer vote from landing).
pub(super) fn on_feedback_done(
    app: &mut App,
    track_id: &str,
    previous: Option<FeedbackVote>,
    result: Result<(), String>,
) -> Vec<Effect> {
    app.autoplay.feedback_inflight.remove(track_id);
    if let Err(e) = result {
        set_vote(app, track_id, previous);
        app.set_status(format!("feedback failed: {e}"), true);
    }
    vec![]
}

/// Set (or, with `None`, clear) the optimistic vote for a track.
fn set_vote(app: &mut App, track_id: &str, vote: Option<FeedbackVote>) {
    match vote {
        Some(v) => {
            app.autoplay.votes.insert(track_id.to_owned(), v);
        }
        None => {
            app.autoplay.votes.remove(track_id);
        }
    }
}
