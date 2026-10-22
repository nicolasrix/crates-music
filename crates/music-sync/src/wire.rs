//! Tagged envelopes for the WebSocket sync protocol.
//!
//! - [`ServerMessage`] is what the gateway pushes to subscribers. The
//!   first frame on a fresh connection is `Snapshot`; every subsequent
//!   frame is `Applied` (or `OpError` / `Ack` / `Pong` for the sender).
//! - [`ClientMessage`] is what clients send up the WS: `Op`, plus an
//!   application-level `Ping` for clients that cannot send protocol ping
//!   frames (browsers).
//!
//! `op_id` is an optional, client-chosen id for one op. A client that
//! applies its ops optimistically uses it to match each op to its ack,
//! and resends unacked ops after a reconnect; the gateway remembers
//! recent ids per room so a resend of an op that already landed is
//! acked (`Ack`) instead of applied twice. Absent, every frame looks
//! exactly as it did before op ids existed.

use serde::{Deserialize, Serialize};

use crate::ops::SyncOp;
use crate::state::SyncState;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Full state, sent immediately after the WS upgrade. Lets a fresh
    /// client converge without first calling `/v1/sync/snapshot`.
    Snapshot { state: SyncState },
    /// An op was just applied. The new `version` is the post-apply
    /// counter; clients can compare against the last known version to
    /// detect (and recover from) gaps.
    Applied {
        op: SyncOp,
        version: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        op_id: Option<String>,
    },
    /// The op the sender just submitted was rejected. Only the sender
    /// receives this — peers see no broadcast, since no state change
    /// happened.
    OpError {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        op_id: Option<String>,
    },
    /// The op carrying `op_id` was already applied earlier (a resend after
    /// a reconnect). Nothing changed and nothing was broadcast; sent only
    /// to the sender so it can stop waiting for an `Applied`.
    Ack { op_id: String },
    /// Reply to [`ClientMessage::Ping`], sent only to the pinger.
    Pong,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Op {
        op: SyncOp,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        op_id: Option<String>,
    },
    /// Liveness probe. A browser can't send protocol ping frames, so a
    /// socket the network silently dropped (phone asleep) still reads
    /// `OPEN`; an unanswered `Ping` is how the client finds out.
    Ping,
}
