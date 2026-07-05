//! Sync-room reducer logic: the TUI as a `/v1/sync` participant.
//!
//! The model is **echo-driven**, exactly like the web client: a queue
//! gesture submits a [`SyncOp`] up the WebSocket and mutates nothing
//! locally; state changes when the server's `applied` echo (or another
//! device's op) arrives, is applied to the [`music_sync::SyncState`]
//! replica, and is *projected* into `app.queue` — which stays the single
//! model the renderer and the audio pipeline read. On a LAN the echo is
//! milliseconds; the audio path was never faster than its byte-fetch
//! anyway, so nothing perceptible waits on the round-trip.
//!
//! [`follow`] runs after every replica change and is the whole
//! device-side policy: it ports the web `PlayerContext`'s dislike
//! auto-skip classifier (fires only when the cursor lands on a *new*
//! track; a direct pick is exempt once; walks in the last advance
//! direction), then drives local rodio toward the room's cursor +
//! `is_playing` — but only while `output_on` ("play audio on this
//! device"). With output off the TUI is a silent remote, same as the
//! web's output toggle.

use music_core::{QueueItem, QueueItemId, SessionId, TrackId};
use music_player::QueuedTrack;
use music_sync::{ServerMessage, SyncOp, SyncState};

use crate::tui::msg::{Effect, SyncEvent};
use crate::tui::signal;
use crate::tui::state::{App, Rating, SyncPhase};

use super::playback::{self, Advance, MoveKind};

// ── server frames ──────────────────────────────────────────────────────

pub(super) fn handle(app: &mut App, ev: SyncEvent) -> Vec<Effect> {
    match ev {
        SyncEvent::Frame(ServerMessage::Snapshot { state }) => adopt_snapshot(app, state),
        SyncEvent::Frame(ServerMessage::Applied { op, version }) => applied(app, &op, version),
        SyncEvent::Frame(ServerMessage::OpError { message }) => {
            // Nothing to roll back — no state was optimistically applied.
            // The op simply didn't happen; say why.
            app.set_status(format!("sync: {message}"), true);
            vec![]
        }
        SyncEvent::Down { reason } => go_offline(app, &reason),
    }
}

/// A snapshot is always adoptable, whenever it arrives: on connect, after
/// a reconnect, mid-connection when the gateway's broadcast lagged, or
/// from an HTTP resync. Server state replaces whatever we had.
fn adopt_snapshot(app: &mut App, state: SyncState) -> Vec<Effect> {
    if !app.sync.online() {
        let note = if app.queue.is_empty() {
            "sync connected — queue is shared"
        } else {
            "sync connected — local queue replaced by the shared one"
        };
        app.set_status(note, false);
    }
    app.sync.phase = SyncPhase::Online;
    app.sync.room = state;
    // Fresh state — anything previously unresolvable is worth retrying.
    app.sync.hydrate_failed.clear();
    project(app);
    follow(app)
}

fn applied(app: &mut App, op: &SyncOp, version: u64) -> Vec<Effect> {
    if !app.sync.online() {
        // Frames while we think we're offline (e.g. after a failed HTTP
        // resync) are a recovery signal: the socket clearly works, so
        // fetch a fresh snapshot and re-adopt.
        return vec![Effect::SyncResync];
    }
    if version <= app.sync.room.version {
        return vec![]; // duplicate/stale (e.g. right after a snapshot)
    }
    if version > app.sync.room.version + 1 {
        tracing::debug!(
            have = app.sync.room.version,
            got = version,
            "sync version gap — resyncing"
        );
        return vec![Effect::SyncResync];
    }
    if let Err(e) = app.sync.room.apply(op, crate::auth::store::now_ms()) {
        // The server applied it and our replica refused: we diverged.
        tracing::debug!(error = %e, "replica diverged — resyncing");
        return vec![Effect::SyncResync];
    }
    app.sync.room.version = version;
    // A queue-growth op may reintroduce a previously-unresolvable id (or a
    // now-restored one) — clear the failed set so hydration retries it,
    // bounding retries to queue changes rather than every frame.
    if matches!(op, SyncOp::Push { .. } | SyncOp::StartSession { .. }) {
        app.sync.hydrate_failed.clear();
    }
    project(app);
    follow(app)
}

fn go_offline(app: &mut App, reason: &str) -> Vec<Effect> {
    match app.sync.phase {
        SyncPhase::Online => {
            app.sync.phase = SyncPhase::Offline;
            // The last projection stays in `app.queue` and simply *is* the
            // local queue now; audio keeps playing. The WS task keeps
            // retrying and the next snapshot re-adopts the shared state.
            //
            // Deliberately keep `last_classified` and `direct_play`: audio
            // is still playing the same track, so on reconnect the snapshot
            // must not treat it as a fresh advance and auto-skip the track
            // the user is actively hearing (a WS blip is not a queue move).
            app.set_status(
                format!("sync offline — queue is local until reconnect ({reason})"),
                true,
            );
        }
        SyncPhase::Offline => tracing::debug!(reason, "sync still offline"),
        SyncPhase::Disabled => {}
    }
    vec![]
}

// ── replica → local model ──────────────────────────────────────────────

/// Rebuild `app.queue` (items + cursor) from the replica, resolving track
/// ids through the metadata map — unhydrated rows show their id until
/// [`hydrate_missing`]'s fetch lands. The queue view's selection is only
/// clamped, never yanked: it's browse state, not room state.
pub(super) fn project(app: &mut App) {
    let items: Vec<QueuedTrack> = app
        .sync
        .room
        .playback
        .queue
        .items
        .iter()
        .map(|it| {
            let id = it.track_id.as_str();
            app.sync.meta.get(id).cloned().unwrap_or_else(|| QueuedTrack {
                id: id.to_owned(),
                title: id.to_owned(),
                artist: None,
                album: None,
                artist_id: None,
                album_id: None,
                duration: None,
            })
        })
        .collect();
    let cursor = app.sync.room.playback.now_playing_index;
    app.queue.set_items(items, cursor);

    let len = app.queue.len();
    match app.queue_table.selected() {
        Some(_) if len == 0 => app.queue_table.select(None),
        Some(s) if s >= len => app.queue_table.select(Some(len - 1)),
        None if len > 0 => app.queue_table.select(Some(0)),
        _ => {}
    }
}

/// Everything this device does in response to the room's state: dislike
/// auto-skip, driving local audio, and requesting metadata hydration.
/// Runs after every replica change; idempotent by construction (every
/// step compares desired vs. actual).
pub(super) fn follow(app: &mut App) -> Vec<Effect> {
    let mut effects = Vec::new();

    // 1. Dislike auto-skip (the web classifier, ported). The device
    //    playing audio owns this policy, so it's gated on `output_on` —
    //    a silent remote never mutates the shared room. It fires only
    //    when the cursor lands on a *new* track (disliking the song that
    //    is playing right now must not yank it) — with one exception:
    //    when a track's metadata *just arrived*, its album/artist verdict
    //    could not be known when it first became current, so hydration
    //    forces a re-classification (`last_classified` is cleared by the
    //    hydration handler before calling us).
    let Some(tid) = app.sync.cursor_track_id().map(str::to_owned) else {
        app.sync.last_classified = None;
        if app.playback.track_id.is_some() && app.sync.output_on {
            app.player_stop();
            app.pending_load = None;
        }
        effects.extend(hydrate_missing(app));
        return effects;
    };
    let advanced = app.sync.last_classified.as_deref() != Some(tid.as_str());
    app.sync.last_classified = Some(tid.clone());
    if app.sync.direct_play.as_deref() == Some(tid.as_str()) {
        // The user explicitly picked it — plays even if disliked.
        app.sync.direct_play = None;
    } else if app.sync.output_on && advanced && track_disliked(app, &tid) {
        let skip_to = next_playable(app);
        effects.extend(hydrate_missing(app));
        app.set_status("auto-skipping a disliked track", false);
        effects.push(Effect::SyncSubmit {
            op: match skip_to {
                Some(target) => SyncOp::SetNowPlaying {
                    index: Some(target),
                },
                // No playable track in this direction — pause rather
                // than sit on a disliked one.
                None => SyncOp::SetPlaying { is_playing: false },
            },
        });
        // Don't load audio for a track we're leaving immediately.
        return effects;
    }

    // 2. Drive local audio toward (cursor, is_playing) — output devices
    //    only. `app.queue` is already the projection, so the existing
    //    load path (prefetch hit, resolve effect, stale guards) applies.
    if app.sync.output_on && !app.no_audio_device {
        let room_playing = app.sync.room.playback.is_playing;
        let loaded = app.playback.track_id.as_deref() == Some(tid.as_str());
        let loading = app
            .pending_load
            .is_some_and(|idx| app.queue.items().get(idx).is_some_and(|t| t.id == tid));
        if loaded {
            if room_playing && !app.playback.playing {
                app.player_resume();
            } else if !room_playing && app.playback.playing {
                app.player_pause();
            }
        } else if !loading {
            if room_playing {
                effects.extend(playback::start_current(app));
            } else if app.playback.track_id.is_some() {
                // Paused room moved its cursor: drop the stale audio so a
                // later local resume can't play the wrong track.
                app.player_stop();
                app.pending_load = None;
            }
        }
    }

    // 3. Hydrate metadata for items we didn't push ourselves.
    effects.extend(hydrate_missing(app));
    effects
}

/// Disliked at the track, album, or artist level — album/artist need the
/// hydrated metadata; before it arrives only the track verdict can match
/// (fail open, like the web while ratings/meta are unknown).
fn track_disliked(app: &App, track_id: &str) -> bool {
    match app.sync.meta.get(track_id) {
        Some(qt) => signal::is_disliked(qt, &app.ratings),
        None => app.ratings.get(track_id) == Some(&Rating::Dislike),
    }
}

/// First non-disliked queue index walking from the cursor in the current
/// advance direction (the web's `nextPlayableIndex`).
fn next_playable(app: &App) -> Option<usize> {
    let items = &app.sync.room.playback.queue.items;
    let from = app.sync.room.playback.now_playing_index?;
    let step = i64::from(app.sync.advance_dir);
    let len = i64::try_from(items.len()).ok()?;
    let mut idx = i64::try_from(from).ok()?;
    loop {
        idx += step;
        if idx < 0 || idx >= len {
            return None;
        }
        let i = usize::try_from(idx).ok()?;
        if !track_disliked(app, items[i].track_id.as_str()) {
            return Some(i);
        }
    }
}

fn hydrate_missing(app: &mut App) -> Vec<Effect> {
    let mut ids: Vec<String> = Vec::new();
    for it in &app.sync.room.playback.queue.items {
        let id = it.track_id.as_str();
        if !app.sync.meta.contains_key(id)
            && !app.sync.hydrating.contains(id)
            && !app.sync.hydrate_failed.contains(id)
            && !ids.iter().any(|seen| seen == id)
        {
            ids.push(id.to_owned());
        }
    }
    if ids.is_empty() {
        return vec![];
    }
    for id in &ids {
        app.sync.hydrating.insert(id.clone());
    }
    vec![Effect::HydrateTracks { ids }]
}

// ── gestures → ops ─────────────────────────────────────────────────────

/// "Play these tracks from `start`" — one atomic `StartSession` op, the
/// same grammar as the web's `playList`/`playSingle`. Picking a track on
/// this device also means "play audio here", so the output toggle flips on.
pub(super) fn start_session(
    app: &mut App,
    queued: &[QueuedTrack],
    start: usize,
) -> Vec<Effect> {
    if queued.is_empty() {
        return vec![];
    }
    let start = start.min(queued.len() - 1);
    let anchor_id = queued[start].id.clone();
    playback::note_abandonment(app, Some(anchor_id.as_str()));
    for t in queued {
        app.sync.meta.insert(t.id.clone(), t.clone());
    }
    app.sync.direct_play = Some(anchor_id);
    app.sync.advance_dir = 1;
    app.sync.output_on = true;
    app.queue_table.select(Some(start));

    let items = queued
        .iter()
        .map(|t| QueueItem {
            item_id: QueueItemId::from(crate::sync::new_item_id()),
            track_id: TrackId::from(t.id.clone()),
        })
        .collect();
    vec![Effect::SyncSubmit {
        op: SyncOp::StartSession {
            items,
            anchor_index: start,
            session_id: SessionId::from(crate::sync::new_session_id()),
        },
    }]
}

/// Append tracks to the room queue (`play_next` reorders each to directly
/// after the cursor, preserving their order — the same landing spots as
/// the local `enqueue_next`).
pub(super) fn push_tracks(
    app: &mut App,
    queued: Vec<QueuedTrack>,
    play_next: bool,
) -> Vec<Effect> {
    let cursor = app.sync.room.playback.now_playing_index;
    let mut effects = Vec::with_capacity(queued.len() * 2);
    for (k, t) in queued.iter().enumerate() {
        let item_id = crate::sync::new_item_id();
        effects.push(Effect::SyncSubmit {
            op: SyncOp::Push {
                item_id: QueueItemId::from(item_id.clone()),
                track_id: TrackId::from(t.id.clone()),
            },
        });
        if play_next {
            // Pushed to the tail, then moved: the target indexes into the
            // queue with the pushed item removed again, so earlier rows
            // are unaffected and consecutive pushes stay in order.
            let target = cursor.map_or(k, |c| c + 1 + k);
            effects.push(Effect::SyncSubmit {
                op: SyncOp::Reorder {
                    item_id: QueueItemId::from(item_id),
                    new_index: target,
                },
            });
        }
    }
    for t in queued {
        app.sync.meta.insert(t.id.clone(), t);
    }
    effects
}

/// Jump the shared cursor to the selected queue row (Enter in the queue
/// view) — a direct pick: exempt from auto-skip, resumes the room if
/// paused, and turns this device's audio on.
pub(super) fn jump_selected(app: &mut App, sel: usize) -> Vec<Effect> {
    let Some(item) = app.sync.room.playback.queue.items.get(sel) else {
        return vec![];
    };
    let tid = item.track_id.as_str().to_owned();
    playback::note_abandonment(app, Some(tid.as_str()));
    app.sync.direct_play = Some(tid);
    app.sync.advance_dir = 1;
    app.sync.output_on = true;

    let mut effects = vec![Effect::SyncSubmit {
        op: SyncOp::SetNowPlaying { index: Some(sel) },
    }];
    if !app.sync.room.playback.is_playing {
        effects.push(Effect::SyncSubmit {
            op: SyncOp::SetPlaying { is_playing: true },
        });
    }
    effects
}

pub(super) fn toggle_playing(app: &mut App) -> Vec<Effect> {
    let pb = &app.sync.room.playback;
    if pb.queue.items.is_empty() {
        return vec![];
    }
    if pb.now_playing_index.is_none() {
        // Shared queue with no cursor (e.g. the playing row was removed):
        // space starts it from the top, like the local idle-restart. Reset
        // the advance direction so auto-skip walks *forward* from track 0
        // (a stale -1 from an earlier `prev` would step off the front).
        app.sync.advance_dir = 1;
        if !app.sync.output_on {
            app.set_status("room started — this device is a silent remote (o for audio)", false);
        }
        return vec![
            Effect::SyncSubmit {
                op: SyncOp::SetNowPlaying { index: Some(0) },
            },
            Effect::SyncSubmit {
                op: SyncOp::SetPlaying { is_playing: true },
            },
        ];
    }
    let resuming = !pb.is_playing;
    if resuming && !app.sync.output_on {
        app.set_status("room resumed — this device is a silent remote (o for audio)", false);
    }
    vec![Effect::SyncSubmit {
        op: SyncOp::SetPlaying {
            is_playing: resuming,
        },
    }]
}

/// n / natural end. Mirrors the web: bounded at the tail (`n` no-ops, a
/// drained track pauses the room; the cursor stays on the last track so
/// prev/space recover it).
pub(super) fn advance(app: &mut App, cause: Advance) -> Vec<Effect> {
    let pb = &app.sync.room.playback;
    let Some(i) = pb.now_playing_index else {
        return vec![];
    };
    app.sync.advance_dir = 1;
    if i + 1 < pb.queue.items.len() {
        let next_id = pb.queue.items[i + 1].track_id.as_str().to_owned();
        if cause == Advance::Manual {
            playback::note_abandonment(app, Some(next_id.as_str()));
        }
        vec![Effect::SyncSubmit {
            op: SyncOp::SetNowPlaying { index: Some(i + 1) },
        }]
    } else if cause == Advance::Natural && pb.is_playing {
        vec![Effect::SyncSubmit {
            op: SyncOp::SetPlaying { is_playing: false },
        }]
    } else {
        vec![]
    }
}

pub(super) fn prev(app: &mut App) -> Vec<Effect> {
    // Deep into a track, "previous" restarts it — device-local, position
    // never syncs (web parity: seeks/position stay on the playing device).
    let cursor_loaded = app
        .sync
        .cursor_track_id()
        .is_some_and(|tid| app.playback.track_id.as_deref() == Some(tid));
    if cursor_loaded && app.playback.position.as_secs() > 3 {
        if let Some(p) = &app.player {
            p.seek_to(std::time::Duration::ZERO);
        }
        return vec![];
    }
    let Some(i) = app.sync.room.playback.now_playing_index else {
        return vec![];
    };
    if i == 0 {
        // Prev at the head restarts (or reloads a dead sink) locally.
        if cursor_loaded {
            if let Some(p) = &app.player {
                p.seek_to(std::time::Duration::ZERO);
            }
            return vec![];
        }
        if app.sync.output_on && app.sync.room.playback.is_playing {
            return playback::start_current(app);
        }
        return vec![];
    }
    let prev_id = app.sync.room.playback.queue.items[i - 1]
        .track_id
        .as_str()
        .to_owned();
    app.sync.advance_dir = -1;
    playback::note_abandonment(app, Some(prev_id.as_str()));
    vec![Effect::SyncSubmit {
        op: SyncOp::SetNowPlaying { index: Some(i - 1) },
    }]
}

pub(super) fn remove_selected(app: &mut App) -> Vec<Effect> {
    let Some(sel) = app.queue_table.selected() else {
        return vec![];
    };
    let Some(item_id) = app
        .sync
        .room
        .playback
        .queue
        .items
        .get(sel)
        .map(|it| it.item_id.clone())
    else {
        return vec![];
    };
    if app.sync.room.playback.now_playing_index == Some(sel) {
        playback::note_abandonment(app, None);
    }
    vec![Effect::SyncSubmit {
        op: SyncOp::Remove { item_id },
    }]
}

/// One `Remove` per non-cursor item (the web's "clear upcoming" — a bare
/// `Clear` would also stop the now-playing track everywhere).
pub(super) fn clear_upcoming(app: &mut App) -> Vec<Effect> {
    let pb = &app.sync.room.playback;
    let effects: Vec<Effect> = pb
        .queue
        .items
        .iter()
        .enumerate()
        .filter(|(i, _)| pb.now_playing_index != Some(*i))
        .map(|(_, item)| Effect::SyncSubmit {
            op: SyncOp::Remove {
                item_id: item.item_id.clone(),
            },
        })
        .collect();
    if !effects.is_empty() {
        app.set_status("cleared upcoming tracks", false);
    }
    effects
}

pub(super) fn reorder_selected(app: &mut App, kind: MoveKind) -> Vec<Effect> {
    let Some(sel) = app.queue_table.selected() else {
        return vec![];
    };
    let pb = &app.sync.room.playback;
    let Some(target) =
        playback::move_target(sel, pb.queue.items.len(), pb.now_playing_index, kind)
    else {
        return vec![];
    };
    let Some(item) = pb.queue.items.get(sel) else {
        return vec![];
    };
    let item_id = item.item_id.clone();
    // The selection follows the row optimistically — it's view-local
    // state, so this doesn't break the echo-driven queue model, and it
    // keeps a repeated J/K acting on the same row.
    app.queue_table.select(Some(target));
    vec![Effect::SyncSubmit {
        op: SyncOp::Reorder {
            item_id,
            new_index: target,
        },
    }]
}

/// `o` — toggle "play audio on this device". Off mid-playback silences
/// immediately; on re-runs [`follow`] so the room's current track starts
/// here. Local/offline mode always plays — there's no room to be a
/// remote *for*.
pub(super) fn toggle_output(app: &mut App) -> Vec<Effect> {
    if !app.sync.online() {
        let msg = match app.sync.phase {
            // No gateway at all — the queue is always local, audio always on.
            SyncPhase::Disabled => "audio output is always on in local mode",
            // Gateway configured but the WS is down right now.
            _ => "audio output toggle needs a live sync connection",
        };
        app.set_status(msg, false);
        return vec![];
    }
    app.sync.output_on = !app.sync.output_on;
    if app.sync.output_on {
        app.set_status("audio output: this device", false);
        follow(app)
    } else {
        app.player_stop();
        app.pending_load = None;
        app.set_status("audio output: off (silent remote)", false);
        vec![]
    }
}
