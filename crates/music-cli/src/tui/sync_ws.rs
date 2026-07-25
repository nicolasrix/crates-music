//! The sync WebSocket task: owns the `/v1/sync` connection for the whole
//! TUI session, forwards every server frame into the message channel, and
//! sends ops handed to it by the reducer (via [`crate::tui::msg::Effect::SyncSubmit`]).
//!
//! Connection lifecycle: connect → frames flow (the first is always a
//! `Snapshot`, which flips the reducer online) → on any error, emit
//! [`SyncEvent::Down`] and retry with capped exponential backoff. The
//! bearer is re-resolved per attempt, so token rotation across a long
//! session (or an overnight disconnect) never wedges the reconnect.
//!
//! Liveness: the gateway only sends frames when ops happen, so a silent
//! wire is normal — we send WS pings on an interval and treat a stretch
//! with no inbound traffic (no pong, no frame) as a dead connection.
//! Without this, a dropped link freezes the queue for as long as the OS
//! takes to notice, which on an idle TCP connection can be minutes.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use music_sync::{ClientMessage, ServerMessage, SyncOp};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use crate::config::Config;

use super::msg::{Msg, SyncEvent};

/// Reconnect backoff: first retry after this…
const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
/// …doubling up to this cap.
const BACKOFF_CAP: Duration = Duration::from_secs(30);
/// Outbound ping cadence while connected.
const PING_EVERY: Duration = Duration::from_secs(10);
/// No inbound traffic (frame or pong) for this long → connection is dead.
const IDLE_DEADLINE: Duration = Duration::from_secs(25);

/// Spawn the WS task. Returns the sender the reducer's `SyncSubmit`
/// effect feeds; the task exits when either the op sender or the msg
/// receiver side goes away (i.e. when the TUI shuts down).
pub(crate) fn spawn(config: Arc<Config>, msg_tx: UnboundedSender<Msg>) -> UnboundedSender<SyncOp> {
    let (op_tx, op_rx) = unbounded_channel::<SyncOp>();
    tokio::spawn(run(config, msg_tx, op_rx));
    op_tx
}

async fn run(config: Arc<Config>, msg_tx: UnboundedSender<Msg>, mut op_rx: UnboundedReceiver<SyncOp>) {
    let mut backoff = BACKOFF_INITIAL;
    loop {
        match connect(&config).await {
            Ok(ws) => {
                backoff = BACKOFF_INITIAL;
                let reason = session(ws, &msg_tx, &mut op_rx).await;
                if send_event(&msg_tx, SyncEvent::Down { reason }).is_err() {
                    return; // TUI is gone
                }
            }
            Err(e) => {
                if send_event(&msg_tx, SyncEvent::Down { reason: e }).is_err() {
                    return;
                }
            }
        }

        // Wait out the backoff. Ops arriving while disconnected are
        // dropped (the reducer runs the queue locally in Offline phase and
        // shouldn't be sending any — this only catches the race where a
        // gesture landed just as the socket died). A drained `None` means
        // the reducer side is gone: exit.
        let deadline = Instant::now() + backoff;
        loop {
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => break,
                op = op_rx.recv() => match op {
                    Some(op) => tracing::debug!(?op, "sync op dropped — disconnected"),
                    None => return,
                },
            }
        }
        backoff = (backoff * 2).min(BACKOFF_CAP);
    }
}

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

/// One connection attempt. Auth mirrors `sync::run_watch`: bearer as
/// `access_token=` query param (RFC 6750 §2.3), token freshly resolved so
/// refresh rotation is picked up.
async fn connect(config: &Config) -> Result<Ws, String> {
    let gw = crate::gateway::require_gateway(config).map_err(|e| e.to_string())?;
    let token = crate::auth::resolve_bearer(config, gw)
        .await
        .map_err(|e| format!("auth: {e}"))?;
    let mut ws_url =
        crate::gateway::ws_url_for(&gw.url, "/v1/sync").map_err(|e| e.to_string())?;
    ws_url.query_pairs_mut().append_pair("access_token", &token);

    let (ws, _resp) = tokio_tungstenite::connect_async(ws_url.as_str())
        .await
        .map_err(|e| format!("connect: {e}"))?;
    Ok(ws)
}

/// Pump one live connection until it dies; returns the reason.
async fn session(
    mut ws: Ws,
    msg_tx: &UnboundedSender<Msg>,
    op_rx: &mut UnboundedReceiver<SyncOp>,
) -> String {
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_rx = Instant::now();

    loop {
        tokio::select! {
            frame = ws.next() => {
                last_rx = Instant::now();
                match frame {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ServerMessage>(&text) {
                            Ok(server_msg) => {
                                if send_event(msg_tx, SyncEvent::Frame(server_msg)).is_err() {
                                    return "shutting down".to_owned();
                                }
                            }
                            Err(e) => tracing::debug!(error = %e, "unparseable sync frame"),
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => return "server closed".to_owned(),
                    Some(Ok(_)) => {} // Ping/Pong/Binary — traffic already noted above
                    Some(Err(e)) => return format!("ws error: {e}"),
                }
            }
            op = op_rx.recv() => match op {
                Some(op) => {
                    let body = match serde_json::to_string(&ClientMessage::Op { op }) {
                        Ok(b) => b,
                        Err(e) => {
                            tracing::debug!(error = %e, "unserializable sync op");
                            continue;
                        }
                    };
                    if let Err(e) = ws.send(Message::Text(body)).await {
                        return format!("send failed: {e}");
                    }
                }
                None => return "shutting down".to_owned(),
            },
            _ = ping.tick() => {
                if last_rx.elapsed() > IDLE_DEADLINE {
                    return "connection unresponsive".to_owned();
                }
                if let Err(e) = ws.send(Message::Ping(Vec::new())).await {
                    return format!("ping failed: {e}");
                }
            }
        }
    }
}

fn send_event(msg_tx: &UnboundedSender<Msg>, ev: SyncEvent) -> Result<(), ()> {
    msg_tx.send(Msg::Sync(ev)).map_err(|_| ())
}
