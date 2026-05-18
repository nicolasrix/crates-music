//! State-machine tests for `SyncState::apply`. Covers the cursor-follow
//! behaviour for remove/reorder, idempotent Push, and the only hard
//! reject (out-of-bounds `SetNowPlaying`).

use music_core::{QueueItem, QueueItemId, SessionId, TrackId};
use music_sync::{ApplyError, SyncOp, SyncState};

/// Sentinel timestamp threaded through every `apply` call in these
/// tests. The state machine is pure: the caller stamps "now", not the
/// state. Tests that care about the actual value override this locally.
const NOW: i64 = 1_710_000_000_000;

fn push(item: &str, track: &str) -> SyncOp {
    SyncOp::Push {
        item_id: QueueItemId::from(item),
        track_id: TrackId::from(track),
    }
}

fn remove(item: &str) -> SyncOp {
    SyncOp::Remove {
        item_id: QueueItemId::from(item),
    }
}

fn reorder(item: &str, new_index: usize) -> SyncOp {
    SyncOp::Reorder {
        item_id: QueueItemId::from(item),
        new_index,
    }
}

fn item_ids(state: &SyncState) -> Vec<String> {
    state
        .playback
        .queue
        .items
        .iter()
        .map(|i| i.item_id.as_str().to_string())
        .collect()
}

#[test]
fn new_state_is_empty_at_version_zero() {
    let s = SyncState::new();
    assert_eq!(s.version, 0);
    assert!(s.playback.queue.items.is_empty());
    assert_eq!(s.playback.now_playing_index, None);
    assert_eq!(s.playback.position_ms, 0);
    assert!(!s.playback.is_playing);
}

#[test]
fn apply_push_appends_and_bumps_version() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    s.apply(&push("b", "t-2"), NOW).unwrap();
    assert_eq!(s.version, 2);
    assert_eq!(item_ids(&s), vec!["a", "b"]);
}

#[test]
fn apply_push_with_existing_item_id_is_idempotent() {
    // Retry-safe: the same Push op replayed must not duplicate the item.
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    assert_eq!(item_ids(&s), vec!["a"]);
    // Version still bumps — every applied op is a discrete event.
    assert_eq!(s.version, 2);
}

#[test]
fn apply_remove_drops_item() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    s.apply(&push("b", "t-2"), NOW).unwrap();
    s.apply(&remove("a"), NOW).unwrap();
    assert_eq!(item_ids(&s), vec!["b"]);
}

#[test]
fn apply_remove_unknown_item_is_no_op_but_bumps_version() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    s.apply(&remove("does-not-exist"), NOW).unwrap();
    assert_eq!(item_ids(&s), vec!["a"]);
    assert_eq!(s.version, 2);
}

#[test]
fn remove_before_cursor_decrements_cursor() {
    // queue: a, b, c (cursor at b == 1) — remove a → cursor follows b
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    s.apply(&push("b", "t-2"), NOW).unwrap();
    s.apply(&push("c", "t-3"), NOW).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(1) }, NOW)
        .unwrap();
    s.apply(&remove("a"), NOW).unwrap();
    assert_eq!(item_ids(&s), vec!["b", "c"]);
    assert_eq!(
        s.playback.now_playing_index,
        Some(0),
        "cursor must follow the same item after a preceding item is removed"
    );
}

#[test]
fn remove_after_cursor_does_not_move_cursor() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    s.apply(&push("b", "t-2"), NOW).unwrap();
    s.apply(&push("c", "t-3"), NOW).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(0) }, NOW)
        .unwrap();
    s.apply(&remove("c"), NOW).unwrap();
    assert_eq!(s.playback.now_playing_index, Some(0));
}

#[test]
fn remove_now_playing_keeps_cursor_pointing_at_next_track() {
    // Removing the now-playing item: cursor stays at same index, which
    // now refers to what was the next track. If queue becomes empty or
    // the cursor would be out of bounds, it's reset to None.
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    s.apply(&push("b", "t-2"), NOW).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(0) }, NOW)
        .unwrap();
    s.apply(&remove("a"), NOW).unwrap();
    assert_eq!(item_ids(&s), vec!["b"]);
    assert_eq!(s.playback.now_playing_index, Some(0));
}

#[test]
fn remove_last_item_clears_cursor() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(0) }, NOW)
        .unwrap();
    s.apply(&remove("a"), NOW).unwrap();
    assert!(s.playback.queue.items.is_empty());
    assert_eq!(s.playback.now_playing_index, None);
}

#[test]
fn reorder_moves_item_to_new_index() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    s.apply(&push("b", "t-2"), NOW).unwrap();
    s.apply(&push("c", "t-3"), NOW).unwrap();
    s.apply(&reorder("c", 0), NOW).unwrap();
    assert_eq!(item_ids(&s), vec!["c", "a", "b"]);
}

#[test]
fn reorder_clamps_new_index_to_valid_range() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    s.apply(&push("b", "t-2"), NOW).unwrap();
    s.apply(&reorder("a", 999), NOW).unwrap();
    assert_eq!(
        item_ids(&s),
        vec!["b", "a"],
        "out-of-range new_index clamps to end"
    );
}

#[test]
fn reorder_unknown_item_is_no_op() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    s.apply(&reorder("does-not-exist", 0), NOW).unwrap();
    assert_eq!(item_ids(&s), vec!["a"]);
}

#[test]
fn reorder_now_playing_item_makes_cursor_follow_it() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    s.apply(&push("b", "t-2"), NOW).unwrap();
    s.apply(&push("c", "t-3"), NOW).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(2) }, NOW)
        .unwrap();
    s.apply(&reorder("c", 0), NOW).unwrap();
    assert_eq!(item_ids(&s), vec!["c", "a", "b"]);
    assert_eq!(s.playback.now_playing_index, Some(0));
}

#[test]
fn reorder_other_item_across_cursor_keeps_cursor_on_same_track() {
    // queue: a, b, c (cursor on b == 1). Move c to index 0 → b drifts to 2.
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    s.apply(&push("b", "t-2"), NOW).unwrap();
    s.apply(&push("c", "t-3"), NOW).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(1) }, NOW)
        .unwrap();
    s.apply(&reorder("c", 0), NOW).unwrap();
    assert_eq!(item_ids(&s), vec!["c", "a", "b"]);
    assert_eq!(
        s.playback.now_playing_index,
        Some(2),
        "cursor must track the same track id, not the same numeric index"
    );
}

#[test]
fn set_now_playing_in_bounds_succeeds() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    s.apply(&push("b", "t-2"), NOW).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(1) }, NOW)
        .unwrap();
    assert_eq!(s.playback.now_playing_index, Some(1));
}

#[test]
fn set_now_playing_out_of_bounds_is_rejected() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    let result = s.apply(&SyncOp::SetNowPlaying { index: Some(5) }, NOW);
    assert!(
        result.is_err(),
        "out-of-bounds SetNowPlaying must be rejected"
    );
    // Rejected ops MUST NOT bump version, otherwise clients drift.
    assert_eq!(s.version, 1, "rejected ops must not bump version");
}

#[test]
fn set_now_playing_to_none_always_succeeds() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(0) }, NOW)
        .unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: None }, NOW)
        .unwrap();
    assert_eq!(s.playback.now_playing_index, None);
}

#[test]
fn set_position_overwrites_position() {
    let mut s = SyncState::new();
    s.apply(&SyncOp::SetPosition { position_ms: 100 }, NOW)
        .unwrap();
    s.apply(&SyncOp::SetPosition { position_ms: 200 }, NOW)
        .unwrap();
    assert_eq!(s.playback.position_ms, 200);
}

#[test]
fn set_playing_lww_last_write_wins() {
    let mut s = SyncState::new();
    s.apply(&SyncOp::SetPlaying { is_playing: true }, NOW)
        .unwrap();
    s.apply(&SyncOp::SetPlaying { is_playing: false }, NOW)
        .unwrap();
    assert!(!s.playback.is_playing);
}

#[test]
fn clear_resets_queue_cursor_position_and_play_flag() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(0) }, NOW)
        .unwrap();
    s.apply(&SyncOp::SetPosition { position_ms: 999 }, NOW)
        .unwrap();
    s.apply(&SyncOp::SetPlaying { is_playing: true }, NOW)
        .unwrap();
    s.apply(&SyncOp::Clear, NOW).unwrap();
    assert!(s.playback.queue.items.is_empty());
    assert_eq!(s.playback.now_playing_index, None);
    assert_eq!(s.playback.position_ms, 0);
    assert!(!s.playback.is_playing);
}

#[test]
fn version_bumps_only_on_accepted_ops() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1"), NOW).unwrap();
    let _ = s.apply(&SyncOp::SetNowPlaying { index: Some(99) }, NOW);
    s.apply(&push("b", "t-2"), NOW).unwrap();
    assert_eq!(s.version, 2);
}

// --- StartSession / StopSession ----------------------------------------

fn item(item_id: &str, track_id: &str) -> QueueItem {
    QueueItem {
        item_id: QueueItemId::from(item_id),
        track_id: TrackId::from(track_id),
    }
}

fn start(items: Vec<QueueItem>, anchor_index: usize, session_id: &str) -> SyncOp {
    SyncOp::StartSession {
        items,
        anchor_index,
        session_id: SessionId::from(session_id),
    }
}

#[test]
fn start_session_replaces_queue_and_sets_anchor() {
    let mut s = SyncState::new();
    // Prior queue + cursor — StartSession is atomic replacement.
    s.apply(&push("old-1", "t-old"), NOW).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(0) }, NOW)
        .unwrap();

    s.apply(
        &start(vec![item("qi-1", "t-1"), item("qi-2", "t-2")], 0, "sess-1"),
        NOW,
    )
    .unwrap();

    assert_eq!(item_ids(&s), vec!["qi-1", "qi-2"]);
    assert_eq!(s.playback.now_playing_index, Some(0));
    let anchor = s.playback.session_anchor.as_ref().expect("anchor set");
    assert_eq!(anchor.session_id.as_str(), "sess-1");
    assert_eq!(anchor.track_id.as_str(), "t-1");
}

#[test]
fn start_session_stamps_started_ms_from_now_ms_param() {
    // Server-stamped: the state machine takes `now_ms`, not the op payload.
    let mut s = SyncState::new();
    let custom_now = 1_700_000_111_111_i64;
    s.apply(&start(vec![item("qi-1", "t-1")], 0, "sess-1"), custom_now)
        .unwrap();
    let anchor = s.playback.session_anchor.as_ref().expect("anchor set");
    assert_eq!(anchor.started_ms, custom_now);
}

#[test]
fn start_session_anchor_index_5_anchors_at_track_5_not_0() {
    // "Play album from track 5": items.len() == 8, anchor_index == 5.
    let mut s = SyncState::new();
    let items: Vec<QueueItem> = (0..8)
        .map(|i| item(&format!("qi-{i}"), &format!("t-{i}")))
        .collect();
    s.apply(&start(items, 5, "sess-1"), NOW).unwrap();
    assert_eq!(s.playback.now_playing_index, Some(5));
    let anchor = s.playback.session_anchor.as_ref().unwrap();
    assert_eq!(anchor.track_id.as_str(), "t-5");
}

#[test]
fn start_session_sets_is_playing_and_resets_position() {
    // Replaces the 4-op pattern (clear + push + set_now_playing + set_playing).
    let mut s = SyncState::new();
    s.apply(
        &SyncOp::SetPosition {
            position_ms: 42_000,
        },
        NOW,
    )
    .unwrap();
    s.apply(&start(vec![item("qi-1", "t-1")], 0, "sess-1"), NOW)
        .unwrap();
    assert!(s.playback.is_playing);
    assert_eq!(s.playback.position_ms, 0);
}

#[test]
fn start_session_with_out_of_range_anchor_index_rejected() {
    let mut s = SyncState::new();
    let result = s.apply(
        &start(vec![item("qi-1", "t-1"), item("qi-2", "t-2")], 5, "sess-1"),
        NOW,
    );
    assert!(matches!(
        result,
        Err(ApplyError::NowPlayingOutOfBounds { .. })
    ));
    // Rejected — no state change, no version bump.
    assert_eq!(s.version, 0);
    assert!(s.playback.queue.items.is_empty());
    assert!(s.playback.session_anchor.is_none());
}

#[test]
fn start_session_with_empty_items_rejected() {
    let mut s = SyncState::new();
    let result = s.apply(&start(vec![], 0, "sess-1"), NOW);
    assert!(result.is_err(), "empty items must not start a session");
    assert_eq!(s.version, 0);
    assert!(s.playback.session_anchor.is_none());
}

#[test]
fn start_session_over_existing_session_replaces_anchor() {
    let mut s = SyncState::new();
    s.apply(&start(vec![item("qi-1", "t-1")], 0, "sess-1"), NOW)
        .unwrap();
    s.apply(&start(vec![item("qi-2", "t-2")], 0, "sess-2"), NOW)
        .unwrap();
    assert_eq!(item_ids(&s), vec!["qi-2"]);
    let anchor = s.playback.session_anchor.as_ref().unwrap();
    assert_eq!(anchor.session_id.as_str(), "sess-2");
    assert_eq!(anchor.track_id.as_str(), "t-2");
}

#[test]
fn stop_session_clears_anchor_only_not_queue_or_cursor() {
    let mut s = SyncState::new();
    s.apply(
        &start(vec![item("qi-1", "t-1"), item("qi-2", "t-2")], 0, "sess-1"),
        NOW,
    )
    .unwrap();
    s.apply(&SyncOp::StopSession, NOW).unwrap();
    assert!(s.playback.session_anchor.is_none(), "anchor must be nulled");
    assert_eq!(
        item_ids(&s),
        vec!["qi-1", "qi-2"],
        "queue must survive StopSession"
    );
    assert_eq!(
        s.playback.now_playing_index,
        Some(0),
        "cursor must survive StopSession"
    );
    assert!(s.playback.is_playing, "play flag must survive StopSession");
}

#[test]
fn clear_op_nulls_anchor() {
    let mut s = SyncState::new();
    s.apply(&start(vec![item("qi-1", "t-1")], 0, "sess-1"), NOW)
        .unwrap();
    s.apply(&SyncOp::Clear, NOW).unwrap();
    assert!(s.playback.session_anchor.is_none());
    assert!(s.playback.queue.items.is_empty());
}

#[test]
fn push_remove_reorder_setnp_setpos_setplaying_leave_anchor_alone() {
    let mut s = SyncState::new();
    s.apply(
        &start(vec![item("qi-1", "t-1"), item("qi-2", "t-2")], 0, "sess-1"),
        NOW,
    )
    .unwrap();
    let original_anchor = s.playback.session_anchor.clone().unwrap();

    s.apply(&push("qi-3", "t-3"), NOW).unwrap();
    s.apply(&remove("qi-2"), NOW).unwrap();
    s.apply(&reorder("qi-3", 0), NOW).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(1) }, NOW)
        .unwrap();
    s.apply(&SyncOp::SetPosition { position_ms: 5_000 }, NOW)
        .unwrap();
    s.apply(&SyncOp::SetPlaying { is_playing: false }, NOW)
        .unwrap();

    assert_eq!(
        s.playback.session_anchor,
        Some(original_anchor),
        "mechanics ops must not touch the session anchor"
    );
}

#[test]
fn stop_session_when_no_anchor_is_idempotent_no_op() {
    let mut s = SyncState::new();
    s.apply(&SyncOp::StopSession, NOW).unwrap();
    s.apply(&SyncOp::StopSession, NOW).unwrap();
    // Two no-op StopSessions still each count as accepted ops.
    assert_eq!(s.version, 2);
    assert!(s.playback.session_anchor.is_none());
}
