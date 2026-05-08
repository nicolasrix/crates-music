//! Wire envelopes for the WebSocket sync protocol. Server pushes
//! state changes as `ServerMessage`; client submits ops as `ClientMessage`.
//! Both use a `"type"` tag with snake_case variant names for symmetry
//! with `SyncOp`.

use music_core::{QueueItemId, TrackId};
use music_sync::{ClientMessage, ServerMessage, SyncOp, SyncState};
use serde_json::json;

#[test]
fn server_snapshot_carries_full_state() {
    let msg = ServerMessage::Snapshot {
        state: SyncState::default(),
    };
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "snapshot");
    assert_eq!(v["state"]["version"], 0);
    assert_eq!(v["state"]["playback"]["queue"]["items"], json!([]));
    let back: ServerMessage = serde_json::from_value(v).unwrap();
    assert_eq!(back, msg);
}

#[test]
fn server_applied_carries_op_and_version() {
    let op = SyncOp::Push {
        item_id: QueueItemId::from("qi-1"),
        track_id: TrackId::from("t-1"),
    };
    let msg = ServerMessage::Applied {
        op: op.clone(),
        version: 42,
    };
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "applied");
    assert_eq!(v["version"], 42);
    assert_eq!(v["op"]["type"], "push");
    let back: ServerMessage = serde_json::from_value(v).unwrap();
    assert_eq!(back, msg);
}

#[test]
fn server_op_error_carries_human_message() {
    let msg = ServerMessage::OpError {
        message: "now_playing index out of bounds".into(),
    };
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "op_error");
    assert_eq!(v["message"], "now_playing index out of bounds");
    let back: ServerMessage = serde_json::from_value(v).unwrap();
    assert_eq!(back, msg);
}

#[test]
fn client_op_message_roundtrips() {
    let msg = ClientMessage::Op {
        op: SyncOp::SetPlaying { is_playing: true },
    };
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "op");
    assert_eq!(v["op"]["type"], "set_playing");
    let back: ClientMessage = serde_json::from_value(v).unwrap();
    assert_eq!(back, msg);
}

#[test]
fn client_unknown_message_type_is_rejected() {
    let v = json!({"type": "ping"});
    let result: Result<ClientMessage, _> = serde_json::from_value(v);
    assert!(result.is_err());
}
