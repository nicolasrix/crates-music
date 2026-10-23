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
        source: None,
    };
    let msg = ServerMessage::Applied {
        op: op.clone(),
        version: 42,
        op_id: None,
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
        op_id: None,
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
        op_id: None,
    };
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "op");
    assert_eq!(v["op"]["type"], "set_playing");
    let back: ClientMessage = serde_json::from_value(v).unwrap();
    assert_eq!(back, msg);
}

#[test]
fn client_unknown_message_type_is_rejected() {
    let v = json!({"type": "hello"});
    let result: Result<ClientMessage, _> = serde_json::from_value(v);
    assert!(result.is_err());
}

#[test]
fn op_id_is_omitted_from_the_wire_when_absent() {
    // Older clients and the REST path never set one; the frame they get
    // back must look exactly like it did before op ids existed.
    let applied = ServerMessage::Applied {
        op: SyncOp::SetPlaying { is_playing: true },
        version: 1,
        op_id: None,
    };
    let v = serde_json::to_value(&applied).unwrap();
    assert!(
        v.get("op_id").is_none(),
        "absent op_id must not serialize: {v}"
    );
}

#[test]
fn client_op_without_op_id_still_parses() {
    let v = json!({"type": "op", "op": {"type": "set_playing", "is_playing": false}});
    let back: ClientMessage = serde_json::from_value(v).unwrap();
    assert_eq!(
        back,
        ClientMessage::Op {
            op: SyncOp::SetPlaying { is_playing: false },
            op_id: None,
        }
    );
}

#[test]
fn op_id_roundtrips_through_op_applied_and_op_error() {
    let op = SyncOp::SetNowPlaying { index: Some(3) };
    let client = ClientMessage::Op {
        op: op.clone(),
        op_id: Some("op-1".into()),
    };
    let v = serde_json::to_value(&client).unwrap();
    assert_eq!(v["op_id"], "op-1");
    assert_eq!(serde_json::from_value::<ClientMessage>(v).unwrap(), client);

    let applied = ServerMessage::Applied {
        op,
        version: 9,
        op_id: Some("op-1".into()),
    };
    let v = serde_json::to_value(&applied).unwrap();
    assert_eq!(v["op_id"], "op-1");
    assert_eq!(serde_json::from_value::<ServerMessage>(v).unwrap(), applied);

    let err = ServerMessage::OpError {
        message: "nope".into(),
        op_id: Some("op-1".into()),
    };
    let v = serde_json::to_value(&err).unwrap();
    assert_eq!(v["op_id"], "op-1");
    assert_eq!(serde_json::from_value::<ServerMessage>(v).unwrap(), err);
}

#[test]
fn ping_pong_and_ack_frames() {
    let from_client: ClientMessage = serde_json::from_value(json!({"type": "ping"})).unwrap();
    assert_eq!(from_client, ClientMessage::Ping);

    let reply = serde_json::to_value(&ServerMessage::Pong).unwrap();
    assert_eq!(reply, json!({"type": "pong"}));

    let ack = ServerMessage::Ack {
        op_id: "op-1".into(),
    };
    let v = serde_json::to_value(&ack).unwrap();
    assert_eq!(v, json!({"type": "ack", "op_id": "op-1"}));
    assert_eq!(serde_json::from_value::<ServerMessage>(v).unwrap(), ack);
}
