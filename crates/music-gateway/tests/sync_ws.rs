//! WebSocket fan-out (P5.3). Drives the router through a real TCP
//! socket because axum's `oneshot` test path doesn't speak the upgrade
//! protocol. Plain HTTP — TLS termination is upstream and orthogonal
//! to the sync logic.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use http::Uri;
use music_gateway::build_router;
use music_sync::{ClientMessage, SyncOp};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::Request as TgRequest},
};

mod common;

/// Bind the router to a random port and return the address.
async fn spawn_app() -> std::net::SocketAddr {
    let app = build_router(common::build_state(common::test_config()).await);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

fn ws_url_with_token(addr: std::net::SocketAddr) -> String {
    format!("ws://{addr}/v1/sync?access_token={}", common::TEST_BEARER)
}

async fn next_text_message(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> serde_json::Value {
    let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws message arrives within 2s")
        .expect("stream not closed")
        .expect("no protocol error");
    match frame {
        Message::Text(t) => serde_json::from_str(&t).expect("server sent valid JSON"),
        other => panic!("expected text frame, got {other:?}"),
    }
}

#[tokio::test]
async fn ws_connect_without_auth_fails() {
    let addr = spawn_app().await;
    // No `?access_token=` and no Authorization header — the bearer guard
    // must reject before the upgrade completes.
    let url: Uri = format!("ws://{addr}/v1/sync").parse().unwrap();
    let req: TgRequest<()> = url.into_client_request().unwrap();
    let result = connect_async(req).await;
    assert!(
        result.is_err(),
        "unauthenticated WS upgrade must be rejected"
    );
}

#[tokio::test]
async fn ws_first_frame_is_snapshot() {
    let addr = spawn_app().await;
    let (mut ws, _resp) = connect_async(ws_url_with_token(addr)).await.unwrap();
    let v = next_text_message(&mut ws).await;
    assert_eq!(v["type"], "snapshot");
    assert_eq!(v["state"]["version"], 0);
    assert_eq!(v["state"]["playback"]["queue"]["items"], json!([]));
}

#[tokio::test]
async fn ws_receives_applied_frame_when_op_posted_via_rest() {
    // The classic cross-channel sync: device A posts via REST, device B
    // is on the WS — B must see the change.
    let app_state = common::build_state(common::test_config()).await;
    let app = build_router(app_state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (mut ws, _) = connect_async(ws_url_with_token(addr)).await.unwrap();
    let snapshot = next_text_message(&mut ws).await;
    assert_eq!(snapshot["type"], "snapshot");

    // Apply the op directly through the same SyncStore the gateway uses.
    let op = SyncOp::Push {
        item_id: music_core::QueueItemId::from("qi-1"),
        track_id: music_core::TrackId::from("t-1"),
    };
    let version = app_state.sync().apply(&op).await.unwrap();
    assert_eq!(version, 1);

    let frame = next_text_message(&mut ws).await;
    assert_eq!(frame["type"], "applied");
    assert_eq!(frame["version"], 1);
    assert_eq!(frame["op"]["type"], "push");
    assert_eq!(frame["op"]["item_id"], "qi-1");
}

#[tokio::test]
async fn ws_op_from_one_client_is_seen_by_a_second() {
    let addr = spawn_app().await;
    let (mut alice, _) = connect_async(ws_url_with_token(addr)).await.unwrap();
    let (mut bob, _) = connect_async(ws_url_with_token(addr)).await.unwrap();
    // Drain both initial snapshots.
    let _ = next_text_message(&mut alice).await;
    let _ = next_text_message(&mut bob).await;

    let client_msg = ClientMessage::Op {
        op: SyncOp::Push {
            item_id: music_core::QueueItemId::from("qi-7"),
            track_id: music_core::TrackId::from("t-7"),
        },
    };
    alice
        .send(Message::Text(serde_json::to_string(&client_msg).unwrap()))
        .await
        .unwrap();

    // Bob must see the Applied frame (broadcast from server).
    let bob_frame = next_text_message(&mut bob).await;
    assert_eq!(bob_frame["type"], "applied");
    assert_eq!(bob_frame["op"]["item_id"], "qi-7");

    // Alice also sees it — sender receives broadcasts too. Useful as ack.
    let alice_frame = next_text_message(&mut alice).await;
    assert_eq!(alice_frame["type"], "applied");
    assert_eq!(alice_frame["op"]["item_id"], "qi-7");
}

#[tokio::test]
async fn ws_invalid_op_returns_op_error_only_to_sender_no_broadcast() {
    let addr = spawn_app().await;
    let (mut alice, _) = connect_async(ws_url_with_token(addr)).await.unwrap();
    let (mut bob, _) = connect_async(ws_url_with_token(addr)).await.unwrap();
    let _ = next_text_message(&mut alice).await; // snapshot
    let _ = next_text_message(&mut bob).await;

    // Out-of-bounds SetNowPlaying — queue is empty.
    let bad = ClientMessage::Op {
        op: SyncOp::SetNowPlaying { index: Some(99) },
    };
    alice
        .send(Message::Text(serde_json::to_string(&bad).unwrap()))
        .await
        .unwrap();

    let alice_frame = next_text_message(&mut alice).await;
    assert_eq!(alice_frame["type"], "op_error");
    assert!(
        alice_frame["message"].is_string(),
        "op_error must include a human-readable message"
    );

    // Bob must NOT see anything — rejected ops do not broadcast.
    let nothing = tokio::time::timeout(Duration::from_millis(200), bob.next()).await;
    assert!(
        nothing.is_err(),
        "rejected op must not produce a broadcast frame: bob received {nothing:?}"
    );
}

#[tokio::test]
async fn ws_malformed_client_message_returns_op_error() {
    let addr = spawn_app().await;
    let (mut ws, _) = connect_async(ws_url_with_token(addr)).await.unwrap();
    let _ = next_text_message(&mut ws).await; // snapshot

    ws.send(Message::Text("totally not valid json".into()))
        .await
        .unwrap();
    let frame = next_text_message(&mut ws).await;
    assert_eq!(frame["type"], "op_error");
}
