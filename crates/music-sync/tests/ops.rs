//! Wire-format tests for `SyncOp`. The op enum is the public protocol
//! between clients and the gateway, so JSON shape changes are
//! breaking-API changes.

use music_core::{QueueItem, QueueItemId, SessionId, TrackId};
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
        source: None,
    };
    assert_eq!(roundtrip(&op), op);
}

#[test]
fn push_op_uses_tagged_type_field() {
    let op = SyncOp::Push {
        item_id: QueueItemId::from("qi-1"),
        track_id: TrackId::from("t-1"),
        source: None,
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

#[test]
fn start_session_op_roundtrips() {
    let op = SyncOp::StartSession {
        items: vec![
            QueueItem {
                item_id: QueueItemId::from("qi-1"),
                track_id: TrackId::from("t-1"),
                source: None,
            },
            QueueItem {
                item_id: QueueItemId::from("qi-2"),
                track_id: TrackId::from("t-2"),
                source: None,
            },
        ],
        anchor_index: 0,
        session_id: SessionId::from("sess-1"),
    };
    assert_eq!(roundtrip(&op), op);
}

#[test]
fn start_session_op_wire_shape() {
    let op = SyncOp::StartSession {
        items: vec![QueueItem {
            item_id: QueueItemId::from("qi-1"),
            track_id: TrackId::from("t-1"),
            source: None,
        }],
        anchor_index: 0,
        session_id: SessionId::from("sess-1"),
    };
    let v: serde_json::Value = serde_json::to_value(&op).unwrap();
    assert_eq!(v["type"], "start_session");
    assert_eq!(v["anchor_index"], 0);
    assert_eq!(v["session_id"], "sess-1");
    assert_eq!(v["items"][0]["item_id"], "qi-1");
    assert_eq!(v["items"][0]["track_id"], "t-1");
}

#[test]
fn stop_session_op_roundtrips_as_tag_only() {
    let op = SyncOp::StopSession;
    let v: serde_json::Value = serde_json::to_value(&op).unwrap();
    assert_eq!(v["type"], "stop_session");
    assert_eq!(roundtrip(&op), op);
}

#[test]
fn push_op_source_is_optional_on_the_wire() {
    // A client that predates `source` sends no field; it must still parse,
    // and an unsourced op must not grow a `"source": null` on the wire.
    let old: SyncOp =
        serde_json::from_str(r#"{"type":"push","item_id":"qi-1","track_id":"t-1"}"#).unwrap();
    let SyncOp::Push { source, .. } = &old else { panic!("not a push") };
    assert!(source.is_none());
    let v = serde_json::to_value(&old).unwrap();
    assert!(v.get("source").is_none());
}

#[test]
fn push_op_with_source_roundtrips() {
    let op = SyncOp::Push {
        item_id: QueueItemId::from("qi-1"),
        track_id: TrackId::from("t-1"),
        source: Some(music_core::PlaySource {
            kind: "album".to_string(),
            id: Some("al-1".to_string()),
        }),
    };
    let v = serde_json::to_value(&op).unwrap();
    assert_eq!(v["source"], serde_json::json!({"kind": "album", "id": "al-1"}));
    assert_eq!(roundtrip(&op), op);
}
