//! WebSocket fan-out for sync.
//!
//! On connect: send a `Snapshot` frame, then forward every `Applied`
//! event from the broadcast channel until the socket closes. Inbound
//! `ClientMessage::Op` frames are applied through the same `SyncStore`
//! that REST uses; rejects (or malformed payloads) come back to the
//! sender as `OpError` and do NOT broadcast. A resend of an op id the
//! room already applied gets a sender-only `Ack`; a `Ping` gets a `Pong`.
//!
//! The socket is bound to the caller's **room** (`principal.room_id()`):
//! it subscribes to that room's bus only and applies inbound ops to it,
//! so a User never sees another User's queue ops (PR C). A guest joins
//! their host's room (PR D).

use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
};
use music_sync::{ClientMessage, ServerMessage, SyncState};
use tokio::sync::broadcast;
use tracing::debug;

use crate::principal::AuthPrincipal;
use crate::state::AppState;
use crate::sync::store::{AppliedEvent, ApplyOutcome, SyncStore};

/// Upper bound on a client op id. Ids are remembered per room for
/// resend de-duplication, so an unbounded one is a memory lever.
const MAX_OP_ID_LEN: usize = 64;

pub async fn ws_handler(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    upgrade: WebSocketUpgrade,
) -> Response {
    let room_id = principal.room_id();
    upgrade.on_upgrade(move |socket| handle_socket(state.sync().clone(), room_id, socket))
}

async fn handle_socket(store: SyncStore, room_id: i64, mut socket: WebSocket) {
    let (snapshot, mut rx) = store.subscribe(room_id).await;
    if send_snapshot(&mut socket, snapshot).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            broadcasted = rx.recv() => {
                if !forward_broadcast(&mut socket, &store, room_id, broadcasted).await {
                    break;
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(t))) => {
                        let keep_open = handle_client_text(&mut socket, &store, room_id, &t).await;
                        if !keep_open {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(e)) => {
                        debug!(error = %e, "ws receive error");
                        break;
                    }
                    // Ignore Ping/Pong/Binary frames — axum handles
                    // ping/pong automatically; we don't speak binary.
                    _ => {}
                }
            }
        }
    }
}

async fn send_snapshot(socket: &mut WebSocket, state: SyncState) -> Result<(), ()> {
    let msg = ServerMessage::Snapshot { state };
    let body = serde_json::to_string(&msg).map_err(|_| ())?;
    socket.send(Message::Text(body)).await.map_err(|_| ())
}

async fn forward_broadcast(
    socket: &mut WebSocket,
    store: &SyncStore,
    room_id: i64,
    msg: Result<AppliedEvent, broadcast::error::RecvError>,
) -> bool {
    match msg {
        Ok(event) => {
            let frame = ServerMessage::Applied {
                op: event.op,
                version: event.version,
                op_id: event.op_id,
            };
            send_text(socket, &frame).await
        }
        Err(broadcast::error::RecvError::Closed) => false,
        Err(broadcast::error::RecvError::Lagged(_)) => {
            // Best effort recovery: send a fresh snapshot so the client
            // can converge to current state without reconnecting.
            let snap = store.snapshot(room_id).await;
            send_text(socket, &ServerMessage::Snapshot { state: snap }).await
        }
    }
}

async fn handle_client_text(
    socket: &mut WebSocket,
    store: &SyncStore,
    room_id: i64,
    text: &str,
) -> bool {
    let parsed: Result<ClientMessage, _> = serde_json::from_str(text);
    match parsed {
        Ok(ClientMessage::Op { op, op_id }) => {
            if op_id
                .as_deref()
                .is_some_and(|id| id.is_empty() || id.len() > MAX_OP_ID_LEN)
            {
                let err = ServerMessage::OpError {
                    message: format!("op_id must be 1-{MAX_OP_ID_LEN} bytes"),
                    op_id: None,
                };
                return send_text(socket, &err).await;
            }
            match store.apply_with_id(room_id, &op, op_id.as_deref()).await {
                // On success the broadcast loop emits Applied; nothing
                // to do here. Sender sees its own op echoed back as an ack.
                Ok(ApplyOutcome::Applied { .. }) => true,
                // A resend of an op that already landed. Nothing was
                // broadcast, so tell the sender directly.
                Ok(ApplyOutcome::Duplicate { .. }) => match op_id {
                    Some(op_id) => send_text(socket, &ServerMessage::Ack { op_id }).await,
                    None => true,
                },
                Err(e) => {
                    let err = ServerMessage::OpError {
                        message: e.to_string(),
                        op_id,
                    };
                    send_text(socket, &err).await
                }
            }
        }
        Ok(ClientMessage::Ping) => send_text(socket, &ServerMessage::Pong).await,
        Err(e) => {
            let err = ServerMessage::OpError {
                message: format!("malformed message: {e}"),
                op_id: None,
            };
            send_text(socket, &err).await
        }
    }
}

async fn send_text(socket: &mut WebSocket, msg: &ServerMessage) -> bool {
    match serde_json::to_string(msg) {
        Ok(body) => socket.send(Message::Text(body)).await.is_ok(),
        Err(_) => false,
    }
}
