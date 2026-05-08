//! Wire-format tests for `SyncOp`. The op enum is the public protocol
//! between clients and the gateway, so JSON shape changes are
//! breaking-API changes.

use music_core::{QueueItemId, TrackId};
use music_sync::SyncOp;

fn roundtrip(op: &SyncOp) -> SyncOp {
    let json = serde_json::to_string(op).unwrap();
    serde_json::from_str(&json).unwrap()
}

#[test]
fn push_op_roundtrips() {
    let op = SyncOp::Push {
        item_id: QueueItemId::from("qi-1"),
        track_id: TrackId::from("t-1"),
    };
    assert_eq!(roundtrip(&op), op);
}

#[test]
fn push_op_uses_tagged_type_field() {
    let op = SyncOp::Push {
        item_id: QueueItemId::from("qi-1"),
        track_id: TrackId::from("t-1"),
    };
    let v: serde_json::Value = serde_json::to_value(&op).unwrap();
    assert_eq!(v["type"], "push");
    assert_eq!(v["item_id"], "qi-1");
    assert_eq!(v["track_id"], "t-1");
}

#[test]
fn remove_op_roundtrips() {
    let op = SyncOp::Remove {
        item_id: QueueItemId::from("qi-9"),
    };
    assert_eq!(roundtrip(&op), op);
}

#[test]
fn reorder_op_roundtrips() {
    let op = SyncOp::Reorder {
        item_id: QueueItemId::from("qi-3"),
        new_index: 0,
    };
    assert_eq!(roundtrip(&op), op);
}

#[test]
fn set_now_playing_with_some_index_roundtrips() {
    let op = SyncOp::SetNowPlaying { index: Some(2) };
    assert_eq!(roundtrip(&op), op);
}

#[test]
fn set_now_playing_with_none_roundtrips() {
    let op = SyncOp::SetNowPlaying { index: None };
    assert_eq!(roundtrip(&op), op);
}

#[test]
fn set_position_roundtrips() {
    let op = SyncOp::SetPosition {
        position_ms: 12_345,
    };
    assert_eq!(roundtrip(&op), op);
}

#[test]
fn set_playing_roundtrips() {
    assert_eq!(
        roundtrip(&SyncOp::SetPlaying { is_playing: true }),
        SyncOp::SetPlaying { is_playing: true }
    );
}

#[test]
fn clear_op_roundtrips_as_tag_only() {
    let op = SyncOp::Clear;
    let v: serde_json::Value = serde_json::to_value(&op).unwrap();
    assert_eq!(v["type"], "clear");
    assert_eq!(roundtrip(&op), op);
}

#[test]
fn unknown_op_type_fails_to_deserialize() {
    let result: Result<SyncOp, _> = serde_json::from_str(r#"{"type":"teleport","x":1}"#);
    assert!(
        result.is_err(),
        "an unknown op tag must not silently succeed: {result:?}"
    );
}
