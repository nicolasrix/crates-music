//! Sync-room reducer tests. Drive the reducer with synthetic
//! `ServerMessage` sequences — no WS, no network. The pure `SyncState`
//! makes every optimistic/echo scenario a plain function call.

use music_core::{PlaybackState, Queue, QueueItem, QueueItemId, Track, TrackId};
use music_sync::{ServerMessage, SyncOp, SyncState};

use super::super::msg::{Effect, Msg, SyncEvent};
use super::super::state::{App, Loadable, Rating, Section, SyncPhase};
use super::update;

fn app() -> App {
    // gateway = true → sync starts Offline (a WS task would flip it online).
    App::new(None, false, true)
}

fn track(id: &str, title: &str) -> Track {
    Track {
        id: TrackId::from(id.to_owned()),
        title: title.to_owned(),
        album_id: None,
        album_name: None,
        artist_id: None,
        artist_name: None,
        track_number: None,
        disc_number: None,
        duration_seconds: Some(180),
        bit_rate_kbps: None,
        content_type: None,
        suffix: None,
        year: None,
        genre: None,
        play_count: None,
        played_at: None,
    }
}

fn item(id: &str) -> QueueItem {
    QueueItem {
        item_id: QueueItemId::from(format!("it-{id}")),
        track_id: TrackId::from(id.to_owned()),
    }
}

/// A room state with the given track ids, cursor, and play flag.
fn room_state(track_ids: &[&str], cursor: Option<usize>, playing: bool, version: u64) -> SyncState {
    SyncState {
        playback: PlaybackState {
            queue: Queue {
                items: track_ids.iter().map(|id| item(id)).collect(),
            },
            now_playing_index: cursor,
            position_ms: 0,
            is_playing: playing,
            session_anchor: None,
        },
        version,
    }
}

fn snapshot(app: &mut App, state: SyncState) -> Vec<Effect> {
    update(app, Msg::Sync(SyncEvent::Frame(ServerMessage::Snapshot { state })))
}

fn applied(app: &mut App, op: SyncOp, version: u64) -> Vec<Effect> {
    update(app, Msg::Sync(SyncEvent::Frame(ServerMessage::Applied { op, version })))
}

/// Online app with output enabled and a hydrated queue — the common
/// "playing on this device" starting point.
fn online_playing(track_ids: &[&str], cursor: usize) -> App {
    let mut a = app();
    snapshot(&mut a, room_state(track_ids, Some(cursor), true, 1));
    a.sync.output_on = true;
    a
}

// ── snapshot adoption / projection ──────────────────────────────────────

#[test]
fn snapshot_flips_online_and_projects_queue() {
    let mut a = app();
    assert_eq!(a.sync.phase, SyncPhase::Offline);
    let fx = snapshot(&mut a, room_state(&["t1", "t2", "t3"], Some(1), true, 5));
    assert_eq!(a.sync.phase, SyncPhase::Online);
    // Queue projected from the replica (ids until hydration lands).
    let ids: Vec<_> = a.queue.items().iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["t1", "t2", "t3"]);
    assert_eq!(a.queue.current_index(), Some(1));
    // Unhydrated → a hydrate effect for the three ids (output off by
    // default, so no audio resolve yet).
    assert!(fx.iter().any(|e| matches!(e, Effect::HydrateTracks { .. })));
}

#[test]
fn hydration_fills_titles_and_reprojects() {
    let mut a = app();
    snapshot(&mut a, room_state(&["t1"], Some(0), false, 1));
    assert_eq!(a.queue.current().unwrap().title, "t1"); // id placeholder
    update(
        &mut a,
        Msg::TracksHydrated {
            ids: vec!["t1".into()],
            result: Ok(vec![track("t1", "Real Title")]),
        },
    );
    assert_eq!(a.queue.current().unwrap().title, "Real Title");
    assert!(!a.sync.hydrating.contains("t1"));
}

// ── applied frames / versioning ─────────────────────────────────────────

#[test]
fn applied_advances_replica_and_projection() {
    let mut a = online_playing(&["t1", "t2"], 0);
    applied(&mut a, SyncOp::Push { item_id: QueueItemId::from("it-t3".to_owned()), track_id: TrackId::from("t3".to_owned()) }, 2);
    assert_eq!(a.sync.room.version, 2);
    assert_eq!(a.queue.len(), 3);
}

#[test]
fn version_gap_triggers_resync() {
    let mut a = online_playing(&["t1"], 0);
    // Jump from v1 straight to v3 — a frame was missed.
    let fx = applied(&mut a, SyncOp::SetPlaying { is_playing: false }, 3);
    assert!(fx.iter().any(|e| matches!(e, Effect::SyncResync)));
    // Replica unchanged until the snapshot re-adopts.
    assert_eq!(a.sync.room.version, 1);
}

#[test]
fn stale_applied_is_ignored() {
    let mut a = online_playing(&["t1", "t2"], 0);
    // A duplicate of an already-seen version (e.g. right after a snapshot).
    let fx = applied(&mut a, SyncOp::SetPlaying { is_playing: false }, 1);
    assert!(fx.is_empty());
    assert_eq!(a.sync.room.version, 1);
}

// ── gestures become ops (no local queue mutation) ───────────────────────

#[test]
fn activate_in_room_submits_set_now_playing_not_local_jump() {
    let mut a = online_playing(&["t1", "t2", "t3"], 0);
    a.section = Section::Queue;
    a.queue_table.select(Some(2));
    let before = a.sync.room.version;
    let fx = update(&mut a, Msg::Activate);
    // The replica hasn't changed — we submitted, we didn't apply.
    assert_eq!(a.sync.room.version, before);
    assert_eq!(a.queue.current_index(), Some(0));
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::SyncSubmit { op: SyncOp::SetNowPlaying { index: Some(2) } }
    )));
}

#[test]
fn manual_next_submits_cursor_advance() {
    let mut a = online_playing(&["t1", "t2"], 0);
    let fx = update(&mut a, Msg::TransportNext);
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::SyncSubmit { op: SyncOp::SetNowPlaying { index: Some(1) } }
    )));
}

#[test]
fn next_at_tail_pauses_the_room() {
    let mut a = online_playing(&["t1"], 0);
    // Natural end at the last track pauses rather than running off.
    let fx = update(&mut a, Msg::Player(music_player::PlayerEvent::TrackEnded));
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::SyncSubmit { op: SyncOp::SetPlaying { is_playing: false } }
    )));
}

#[test]
fn queue_move_down_submits_reorder() {
    let mut a = online_playing(&["t1", "t2", "t3"], 0);
    a.section = Section::Queue;
    a.queue_table.select(Some(0));
    let fx = update(&mut a, Msg::QueueMoveDown);
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::SyncSubmit { op: SyncOp::Reorder { new_index: 1, .. } }
    )));
    // Selection follows the row optimistically.
    assert_eq!(a.queue_table.selected(), Some(1));
}

#[test]
fn clear_upcoming_removes_every_non_cursor_item() {
    let mut a = online_playing(&["t1", "t2", "t3"], 1);
    a.section = Section::Queue;
    let fx = update(&mut a, Msg::QueueClear);
    let removes: Vec<_> = fx
        .iter()
        .filter_map(|e| match e {
            Effect::SyncSubmit { op: SyncOp::Remove { item_id } } => Some(item_id.as_str().to_owned()),
            _ => None,
        })
        .collect();
    // t1 (idx0) and t3 (idx2) removed; t2 (cursor) kept.
    assert_eq!(removes.len(), 2);
    assert!(removes.contains(&"it-t1".to_owned()));
    assert!(removes.contains(&"it-t3".to_owned()));
    assert!(!removes.contains(&"it-t2".to_owned()));
}

#[test]
fn play_new_queue_online_submits_start_session() {
    let mut a = online_playing(&["old"], 0);
    a.section = Section::Stations;
    a.stations.results = Loadable::Ready(vec![
        track("n1", "New 1"),
        track("n2", "New 2"),
    ]);
    a.stations.table.select(Some(1));
    let fx = update(&mut a, Msg::Activate);
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::SyncSubmit { op: SyncOp::StartSession { anchor_index: 1, .. } }
    )));
    // Anchor is a direct pick → exempt from auto-skip, output turns on.
    assert!(a.sync.output_on);
    assert_eq!(a.sync.direct_play.as_deref(), Some("n2"));
}

// ── dislike auto-skip on the room path ──────────────────────────────────

#[test]
fn auto_skip_disliked_cursor_on_advance() {
    let mut a = online_playing(&["t1", "t2", "t3"], 0);
    a.ratings.insert("t2".into(), Rating::Dislike);
    // Server advances the cursor onto the disliked t2.
    let fx = applied(&mut a, SyncOp::SetNowPlaying { index: Some(1) }, 2);
    // We submit a skip past it to t3.
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::SyncSubmit { op: SyncOp::SetNowPlaying { index: Some(2) } }
    )));
}

#[test]
fn disliking_current_track_does_not_yank_it() {
    let mut a = online_playing(&["t1", "t2"], 0);
    // Dislike the track that's playing right now, then a benign frame
    // re-runs follow: it must not auto-skip the in-place dislike.
    a.ratings.insert("t1".into(), Rating::Dislike);
    let fx = applied(&mut a, SyncOp::SetPosition { position_ms: 1000 }, 2);
    assert!(!fx.iter().any(|e| matches!(
        e,
        Effect::SyncSubmit { op: SyncOp::SetNowPlaying { .. } }
    )));
}

#[test]
fn direct_pick_overrides_dislike() {
    let mut a = online_playing(&["t1", "t2"], 0);
    a.ratings.insert("t2".into(), Rating::Dislike);
    a.section = Section::Queue;
    a.queue_table.select(Some(1));
    // Directly activating disliked t2 marks it exempt…
    update(&mut a, Msg::Activate);
    assert_eq!(a.sync.direct_play.as_deref(), Some("t2"));
    // …so when the server echoes the cursor move, follow lets it play.
    let fx = applied(&mut a, SyncOp::SetNowPlaying { index: Some(1) }, 2);
    assert!(!fx.iter().any(|e| matches!(
        e,
        Effect::SyncSubmit { op: SyncOp::SetNowPlaying { .. } }
    )));
    assert!(a.sync.direct_play.is_none()); // consumed
}

// ── output toggle / silent remote ───────────────────────────────────────

#[test]
fn output_off_makes_it_a_silent_remote() {
    let mut a = online_playing(&["t1"], 0);
    a.section = Section::Queue;
    // Turn output off — no audio, but ops still flow.
    update(&mut a, Msg::ToggleOutput);
    assert!(!a.sync.output_on);
    a.queue_table.select(Some(0));
    let fx = update(&mut a, Msg::QueueMoveTop); // no-op move, but online
    let _ = fx;
    // A cursor-advancing frame produces no audio resolve while output off.
    let fx = applied(&mut a, SyncOp::SetPosition { position_ms: 500 }, 2);
    assert!(!fx.iter().any(|e| matches!(e, Effect::ResolveAudio { .. })));
}

// ── degraded mode ───────────────────────────────────────────────────────

#[test]
fn ws_down_drops_to_offline_but_keeps_queue() {
    let mut a = online_playing(&["t1", "t2"], 0);
    assert_eq!(a.queue.len(), 2);
    update(&mut a, Msg::Sync(SyncEvent::Down { reason: "boom".into() }));
    assert_eq!(a.sync.phase, SyncPhase::Offline);
    // The projected queue survives as the local queue.
    assert_eq!(a.queue.len(), 2);
    // And a local gesture now mutates locally instead of submitting.
    a.section = Section::Queue;
    a.queue_table.select(Some(0));
    let fx = update(&mut a, Msg::QueueMoveDown);
    assert!(!fx.iter().any(|e| matches!(e, Effect::SyncSubmit { .. })));
    let ids: Vec<_> = a.queue.items().iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["t2", "t1"]);
}

#[test]
fn reconnect_snapshot_readopts_server_state() {
    let mut a = online_playing(&["t1", "t2"], 0);
    update(&mut a, Msg::Sync(SyncEvent::Down { reason: "drop".into() }));
    // Server queue changed while we were away; reconnect snapshot wins.
    snapshot(&mut a, room_state(&["x1", "x2", "x3"], Some(2), true, 9));
    assert_eq!(a.sync.phase, SyncPhase::Online);
    let ids: Vec<_> = a.queue.items().iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["x1", "x2", "x3"]);
    assert_eq!(a.queue.current_index(), Some(2));
}

