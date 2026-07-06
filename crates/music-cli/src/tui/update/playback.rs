//! Playback + queue reducer logic. Every gesture entry point forks on the
//! sync phase: online routes to [`super::room`] (the gesture becomes a
//! submitted op; state changes when the server echo arrives), otherwise
//! the local [`music_player::PlayQueue`] is mutated directly — the
//! degraded/direct-mode behavior, unchanged from before the sync-room
//! integration.

use crate::tui::msg::Effect;
use crate::tui::signal::{self, MAX_EVENT_ATTEMPTS, PendingEvent, ScrobbleDecision, TrackSignal};
use crate::tui::state::App;

use super::room;

/// How the queue moved off the current track: a user gesture (skip signal)
/// or the track draining / failing on its own (no signal).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Advance {
    Manual,
    Natural,
}

/// Which slot a queue-move key targets.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MoveKind {
    Down,
    Up,
    Top,
    /// Directly after the now-playing cursor ("play next").
    AfterCursor,
}

/// Event-outbox flush cadence in ticks (~5 s at the 250 ms tick).
const FLUSH_EVERY_TICKS: u64 = 20;

// ── listening signal ────────────────────────────────────────────────────

/// Per-tick signal work: keep the per-track emission state in step with
/// what the player reports, fire due scrobbles, and flush the event outbox
/// on its cadence.
pub(super) fn signal_tick(app: &mut App) -> Vec<Effect> {
    let mut effects = Vec::new();

    if let Some(id) = app.playback.track_id.clone() {
        if app.signal.as_ref().is_none_or(|s| s.track_id != id) {
            app.signal = Some(TrackSignal::new(id.clone()));
        }
        let sig = app.signal.as_mut().expect("just ensured above");
        match signal::evaluate_scrobble(app.playback.duration, app.playback.position, sig) {
            ScrobbleDecision::NowPlaying => {
                sig.now_playing_sent = true;
                effects.push(Effect::Scrobble {
                    track_id: id,
                    submission: false,
                });
            }
            ScrobbleDecision::Submission => {
                sig.submission_sent = true;
                effects.push(Effect::Scrobble {
                    track_id: id,
                    submission: true,
                });
            }
            ScrobbleDecision::None => {}
        }
    }

    if !app.events_outbox.is_empty()
        && !app.events_inflight
        && app.tick.is_multiple_of(FLUSH_EVERY_TICKS)
    {
        app.events_inflight = true;
        effects.push(Effect::FlushEvents {
            events: std::mem::take(&mut app.events_outbox),
        });
    }
    effects
}

/// The current track is being *manually* abandoned (next/prev, activating
/// another track, removing or clearing it). Record at most one skip verdict
/// per load — the gate itself (too short / never started) lives in
/// [`signal::evaluate_skip`]. Natural end-of-track never comes through here.
///
/// `next_id` is the track about to start, when known: restarting the same
/// track is not a skip (mirrors the web player's guard).
pub(super) fn note_abandonment(app: &mut App, next_id: Option<&str>) {
    if !app.events_enabled {
        return;
    }
    let Some(id) = app.playback.track_id.clone() else {
        return;
    };
    if next_id == Some(id.as_str()) {
        return;
    }
    if app
        .signal
        .as_ref()
        .is_some_and(|s| s.track_id == id && s.abandoned)
    {
        return;
    }
    if let Some(played_ms) = signal::evaluate_skip(app.playback.duration, app.playback.position) {
        app.events_outbox.push(PendingEvent::skip(id.clone(), played_ms));
    }
    // Mark the verdict taken even when gated out — one decision per load.
    match app.signal.as_mut() {
        Some(s) if s.track_id == id => s.abandoned = true,
        _ => {
            let mut s = TrackSignal::new(id);
            s.abandoned = true;
            app.signal = Some(s);
        }
    }
}

/// Put failed events back at the front of the outbox (order preserved),
/// dropping any that exhausted their attempts.
pub(super) fn requeue_failed_events(app: &mut App, events: Vec<PendingEvent>, error: &str) {
    tracing::debug!(count = events.len(), error, "event flush failed");
    let mut retained: Vec<PendingEvent> = events
        .into_iter()
        .filter_map(|mut ev| {
            ev.attempts += 1;
            (ev.attempts < MAX_EVENT_ATTEMPTS).then_some(ev)
        })
        .collect();
    retained.append(&mut app.events_outbox);
    app.events_outbox = retained;
}

// ── loading the current track ──────────────────────────────────────────

/// Load-and-play the queue's current track: prefetch hit loads instantly,
/// otherwise a resolve effect goes out and `pending_load` marks the wait.
/// In room mode this runs *after* projection (from [`room::follow`]), so
/// `app.queue` already mirrors the server's cursor.
pub(super) fn start_current(app: &mut App) -> Vec<Effect> {
    if app.no_audio_device {
        app.set_status("no audio device — playback disabled", true);
        return vec![];
    }
    let Some(idx) = app.queue.current_index() else {
        return vec![];
    };
    let Some(cur) = app.queue.current() else {
        return vec![];
    };
    let (id, duration) = (cur.id.clone(), cur.duration);

    if app.prefetched.as_ref().is_some_and(|(tid, _)| *tid == id) {
        let (tid, bytes) = app.prefetched.take().expect("checked above");
        app.player_load(bytes, tid, duration);
        app.pending_load = None;
        return prefetch_next(app);
    }
    app.pending_load = Some(idx);
    vec![Effect::ResolveAudio {
        queue_index: idx,
        track_id: id,
    }]
}

pub(super) fn prefetch_next(app: &mut App) -> Vec<Effect> {
    match app.queue.next_up() {
        Some(next) if app.prefetched.as_ref().is_none_or(|(tid, _)| *tid != next.id) => {
            vec![Effect::PrefetchAudio {
                track_id: next.id.clone(),
            }]
        }
        _ => vec![],
    }
}

// ── transport ──────────────────────────────────────────────────────────

pub(super) fn transport_toggle(app: &mut App) -> Vec<Effect> {
    if app.sync.online() {
        return room::toggle_playing(app);
    }
    // Idle with a queue: (re)start rather than toggling a dead sink.
    if app.playback.track_id.is_none() && app.pending_load.is_none() {
        if app.queue.current().is_none() && !app.queue.is_empty() {
            app.queue.jump(0);
        }
        if app.queue.current().is_some() {
            // Idle restart is not a direct pick — honor dislikes.
            return start_current_playable(app);
        }
        return vec![];
    }
    if let Some(p) = &app.player {
        p.toggle();
    }
    vec![]
}

pub(super) fn next_track(app: &mut App, cause: Advance) -> Vec<Effect> {
    if app.sync.online() {
        return room::advance(app, cause);
    }
    if cause == Advance::Manual {
        note_abandonment(app, None);
    }
    if app.queue.advance().is_none() {
        // Ran off the end: playback stops naturally; clear transients.
        app.pending_load = None;
        app.player_stop();
        return vec![];
    }
    start_current_playable(app)
}

/// [`start_current`] honoring dislike auto-skip — for every path where the
/// queue lands on a track *by itself* (advance, removal slide-in, idle
/// restart) rather than by a direct pick. Walking off the end because all
/// that remains is disliked is the same terminal state as running off
/// naturally.
fn start_current_playable(app: &mut App) -> Vec<Effect> {
    auto_skip_forward(app);
    if app.queue.current().is_some() {
        start_current(app)
    } else {
        app.pending_load = None;
        app.player_stop();
        vec![]
    }
}

/// Dislike auto-skip: when the queue *advances onto* a disliked track
/// (track, album, or artist verdict), walk forward to the first playable
/// one. Direct picks (activating a specific row) never come through here —
/// an explicit choice overrides the dislike, mirroring the web player.
fn auto_skip_forward(app: &mut App) {
    let mut skipped = 0usize;
    while app
        .queue
        .current()
        .is_some_and(|t| signal::is_disliked(t, &app.ratings))
    {
        skipped += 1;
        if app.queue.advance().is_none() {
            break;
        }
    }
    if skipped > 0 {
        app.set_status(format!("auto-skipped {skipped} disliked track(s)"), false);
        sync_queue_cursor(app);
    }
}

pub(super) fn prev_track(app: &mut App) -> Vec<Effect> {
    if app.sync.online() {
        return room::prev(app);
    }
    // Deep into a track, "previous" means restart (the standard transport
    // behavior); near the start it goes to the prior track.
    if app.playback.position.as_secs() > 3 {
        if let Some(p) = &app.player {
            p.seek_to(std::time::Duration::ZERO);
        }
        return vec![];
    }
    // Walk backward over disliked tracks to the first playable target.
    let not_disliked = |i: &usize| !signal::is_disliked(&app.queue.items()[*i], &app.ratings);
    let target = match app.queue.current_index() {
        Some(cur) => (0..cur).rev().find(not_disliked),
        // Finished queue: "previous" recovers the tail, minus dislikes.
        None => (0..app.queue.len()).rev().find(not_disliked),
    };
    let Some(target) = target else {
        // Nothing playable behind: restart the current track (the standard
        // prev-at-start behavior). A live sink just seeks; an idle or
        // failed one needs the full reload to recover.
        let current_loaded = app
            .queue
            .current()
            .is_some_and(|t| app.playback.track_id.as_deref() == Some(t.id.as_str()));
        if current_loaded {
            if let Some(p) = &app.player {
                p.seek_to(std::time::Duration::ZERO);
            }
            return vec![];
        }
        return if app.queue.current().is_some() {
            start_current(app)
        } else {
            vec![]
        };
    };
    note_abandonment(app, None);
    if app.queue.jump(target).is_some() {
        sync_queue_cursor(app);
        start_current(app)
    } else {
        vec![]
    }
}

pub(super) fn player_event(app: &mut App, ev: music_player::PlayerEvent) -> Vec<Effect> {
    match ev {
        music_player::PlayerEvent::TrackEnded => next_track(app, Advance::Natural),
        music_player::PlayerEvent::Error(e) => {
            app.set_status(e, true);
            vec![]
        }
    }
}

// ── queue edits ────────────────────────────────────────────────────────

pub(super) fn queue_remove_selected(app: &mut App) -> Vec<Effect> {
    if app.sync.online() {
        return room::remove_selected(app);
    }
    let Some(sel) = app.queue_table.selected() else {
        return vec![];
    };
    let was_current = app.queue.current_index() == Some(sel);
    if was_current {
        note_abandonment(app, None);
    }
    app.queue.remove(sel);
    let len = app.queue.len();
    if len == 0 {
        app.queue_table.select(None);
        if was_current {
            app.player_stop();
            app.pending_load = None;
        }
        return vec![];
    }
    app.queue_table.select(Some(sel.min(len - 1)));
    if was_current {
        // The next track slid into the cursor slot — play it (honoring
        // dislikes: the slide-in was not a direct pick).
        return start_current_playable(app);
    }
    vec![]
}

/// Clear *upcoming* tracks, preserving the now-playing one (and playback).
/// With nothing playing this empties the queue. Mirrors the web queue
/// page's "clear upcoming" — a full wipe mid-listen was never what the
/// gesture meant.
pub(super) fn queue_clear_upcoming(app: &mut App) -> Vec<Effect> {
    if app.sync.online() {
        return room::clear_upcoming(app);
    }
    if let Some(current) = app.queue.current().cloned() {
        app.queue.set_items(vec![current], Some(0));
        app.queue_table.select(Some(0));
    } else {
        app.queue.clear();
        app.queue_table.select(None);
    }
    app.prefetched = None;
    app.set_status("cleared upcoming tracks", false);
    vec![]
}

/// Move the selected queue row (J/K/T/P in the queue view). The selection
/// follows the moved row so repeated presses keep acting on it.
pub(super) fn queue_move(app: &mut App, kind: MoveKind) -> Vec<Effect> {
    if app.sync.online() {
        return room::reorder_selected(app, kind);
    }
    let Some(sel) = app.queue_table.selected() else {
        return vec![];
    };
    let Some(target) = move_target(sel, app.queue.len(), app.queue.current_index(), kind) else {
        return vec![];
    };
    app.queue.move_item(sel, target);
    app.queue_table.select(Some(target));
    vec![]
}

/// Where a [`MoveKind`] sends the row at `sel`, or `None` for a no-op
/// (already there, at the boundary, or moving the cursor row after
/// itself). Shared by the local and room paths so both modes reorder
/// identically.
pub(super) fn move_target(
    sel: usize,
    len: usize,
    cursor: Option<usize>,
    kind: MoveKind,
) -> Option<usize> {
    if sel >= len {
        return None;
    }
    match kind {
        MoveKind::Down => (sel + 1 < len).then_some(sel + 1),
        MoveKind::Up => sel.checked_sub(1),
        MoveKind::Top => (sel > 0).then_some(0),
        MoveKind::AfterCursor => {
            let c = cursor?;
            if sel == c {
                return None;
            }
            // Reorder indexes into the list *after* removal: removing a
            // row above the cursor shifts the cursor left by one.
            Some(if sel < c { c } else { c + 1 })
        }
    }
}

/// Keep the queue view's cursor on the playing track after a replace/jump.
pub(super) fn sync_queue_cursor(app: &mut App) {
    app.queue_table.select(app.queue.current_index());
}

/// Replace the queue and play from `start` — the shared tail of every
/// "pick a track from a list" activate arm. A direct pick plays even a
/// disliked track, and restarting the track that is already playing is
/// not a skip.
pub(super) fn play_new_queue(
    app: &mut App,
    queued: Vec<music_player::QueuedTrack>,
    start: usize,
) -> Vec<Effect> {
    if app.sync.online() {
        return room::start_session(app, &queued, start);
    }
    let next_id = queued.get(start).map(|t| t.id.clone());
    note_abandonment(app, next_id.as_deref());
    app.queue.replace(queued, start);
    app.prefetched = None;
    sync_queue_cursor(app);
    start_current(app)
}

/// Append tracks to the queue (or push them into the room). The status
/// line is the caller's job — enqueue sources word it differently.
pub(super) fn enqueue_tracks(
    app: &mut App,
    queued: Vec<music_player::QueuedTrack>,
    play_next: bool,
) -> Vec<Effect> {
    if queued.is_empty() {
        return vec![];
    }
    if app.sync.online() {
        return room::push_tracks(app, queued, play_next);
    }
    if play_next {
        app.queue.enqueue_next(queued);
    } else {
        app.queue.enqueue(queued);
    }
    vec![]
}
