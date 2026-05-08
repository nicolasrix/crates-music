//! State-machine tests for `SyncState::apply`. Covers the cursor-follow
//! behaviour for remove/reorder, idempotent Push, and the only hard
//! reject (out-of-bounds `SetNowPlaying`).

use music_core::{QueueItemId, TrackId};
use music_sync::{SyncOp, SyncState};

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
    s.apply(&push("a", "t-1")).unwrap();
    s.apply(&push("b", "t-2")).unwrap();
    assert_eq!(s.version, 2);
    assert_eq!(item_ids(&s), vec!["a", "b"]);
}

#[test]
fn apply_push_with_existing_item_id_is_idempotent() {
    // Retry-safe: the same Push op replayed must not duplicate the item.
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1")).unwrap();
    s.apply(&push("a", "t-1")).unwrap();
    assert_eq!(item_ids(&s), vec!["a"]);
    // Version still bumps — every applied op is a discrete event.
    assert_eq!(s.version, 2);
}

#[test]
fn apply_remove_drops_item() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1")).unwrap();
    s.apply(&push("b", "t-2")).unwrap();
    s.apply(&remove("a")).unwrap();
    assert_eq!(item_ids(&s), vec!["b"]);
}

#[test]
fn apply_remove_unknown_item_is_no_op_but_bumps_version() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1")).unwrap();
    s.apply(&remove("does-not-exist")).unwrap();
    assert_eq!(item_ids(&s), vec!["a"]);
    assert_eq!(s.version, 2);
}

#[test]
fn remove_before_cursor_decrements_cursor() {
    // queue: a, b, c (cursor at b == 1) — remove a → cursor follows b
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1")).unwrap();
    s.apply(&push("b", "t-2")).unwrap();
    s.apply(&push("c", "t-3")).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(1) }).unwrap();
    s.apply(&remove("a")).unwrap();
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
    s.apply(&push("a", "t-1")).unwrap();
    s.apply(&push("b", "t-2")).unwrap();
    s.apply(&push("c", "t-3")).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(0) }).unwrap();
    s.apply(&remove("c")).unwrap();
    assert_eq!(s.playback.now_playing_index, Some(0));
}

#[test]
fn remove_now_playing_keeps_cursor_pointing_at_next_track() {
    // Removing the now-playing item: cursor stays at same index, which
    // now refers to what was the next track. If queue becomes empty or
    // the cursor would be out of bounds, it's reset to None.
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1")).unwrap();
    s.apply(&push("b", "t-2")).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(0) }).unwrap();
    s.apply(&remove("a")).unwrap();
    assert_eq!(item_ids(&s), vec!["b"]);
    assert_eq!(s.playback.now_playing_index, Some(0));
}

#[test]
fn remove_last_item_clears_cursor() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1")).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(0) }).unwrap();
    s.apply(&remove("a")).unwrap();
    assert!(s.playback.queue.items.is_empty());
    assert_eq!(s.playback.now_playing_index, None);
}

#[test]
fn reorder_moves_item_to_new_index() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1")).unwrap();
    s.apply(&push("b", "t-2")).unwrap();
    s.apply(&push("c", "t-3")).unwrap();
    s.apply(&reorder("c", 0)).unwrap();
    assert_eq!(item_ids(&s), vec!["c", "a", "b"]);
}

#[test]
fn reorder_clamps_new_index_to_valid_range() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1")).unwrap();
    s.apply(&push("b", "t-2")).unwrap();
    s.apply(&reorder("a", 999)).unwrap();
    assert_eq!(
        item_ids(&s),
        vec!["b", "a"],
        "out-of-range new_index clamps to end"
    );
}

#[test]
fn reorder_unknown_item_is_no_op() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1")).unwrap();
    s.apply(&reorder("does-not-exist", 0)).unwrap();
    assert_eq!(item_ids(&s), vec!["a"]);
}

#[test]
fn reorder_now_playing_item_makes_cursor_follow_it() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1")).unwrap();
    s.apply(&push("b", "t-2")).unwrap();
    s.apply(&push("c", "t-3")).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(2) }).unwrap();
    s.apply(&reorder("c", 0)).unwrap();
    assert_eq!(item_ids(&s), vec!["c", "a", "b"]);
    assert_eq!(s.playback.now_playing_index, Some(0));
}

#[test]
fn reorder_other_item_across_cursor_keeps_cursor_on_same_track() {
    // queue: a, b, c (cursor on b == 1). Move c to index 0 → b drifts to 2.
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1")).unwrap();
    s.apply(&push("b", "t-2")).unwrap();
    s.apply(&push("c", "t-3")).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(1) }).unwrap();
    s.apply(&reorder("c", 0)).unwrap();
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
    s.apply(&push("a", "t-1")).unwrap();
    s.apply(&push("b", "t-2")).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(1) }).unwrap();
    assert_eq!(s.playback.now_playing_index, Some(1));
}

#[test]
fn set_now_playing_out_of_bounds_is_rejected() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1")).unwrap();
    let result = s.apply(&SyncOp::SetNowPlaying { index: Some(5) });
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
    s.apply(&push("a", "t-1")).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(0) }).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: None }).unwrap();
    assert_eq!(s.playback.now_playing_index, None);
}

#[test]
fn set_position_overwrites_position() {
    let mut s = SyncState::new();
    s.apply(&SyncOp::SetPosition { position_ms: 100 }).unwrap();
    s.apply(&SyncOp::SetPosition { position_ms: 200 }).unwrap();
    assert_eq!(s.playback.position_ms, 200);
}

#[test]
fn set_playing_lww_last_write_wins() {
    let mut s = SyncState::new();
    s.apply(&SyncOp::SetPlaying { is_playing: true }).unwrap();
    s.apply(&SyncOp::SetPlaying { is_playing: false }).unwrap();
    assert!(!s.playback.is_playing);
}

#[test]
fn clear_resets_queue_cursor_position_and_play_flag() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1")).unwrap();
    s.apply(&SyncOp::SetNowPlaying { index: Some(0) }).unwrap();
    s.apply(&SyncOp::SetPosition { position_ms: 999 }).unwrap();
    s.apply(&SyncOp::SetPlaying { is_playing: true }).unwrap();
    s.apply(&SyncOp::Clear).unwrap();
    assert!(s.playback.queue.items.is_empty());
    assert_eq!(s.playback.now_playing_index, None);
    assert_eq!(s.playback.position_ms, 0);
    assert!(!s.playback.is_playing);
}

#[test]
fn version_bumps_only_on_accepted_ops() {
    let mut s = SyncState::new();
    s.apply(&push("a", "t-1")).unwrap();
    let _ = s.apply(&SyncOp::SetNowPlaying { index: Some(99) });
    s.apply(&push("b", "t-2")).unwrap();
    assert_eq!(s.version, 2);
}
