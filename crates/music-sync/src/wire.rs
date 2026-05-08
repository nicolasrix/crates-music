//! Tagged envelopes for the WebSocket sync protocol.
//!
//! - [`ServerMessage`] is what the gateway pushes to subscribers. The
//!   first frame on a fresh connection is `Snapshot`; every subsequent
//!   frame is `Applied` (or `OpError` for the sender of a rejected op).
//! - [`ClientMessage`] is what clients send up the WS. Only `Op` is
//!   defined for now; `Ping`/`Hello` style frames can be added without
//!   breaking the on-the-wire shape.

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
    Applied { op: SyncOp, version: u64 },
    /// The op the sender just submitted was rejected. Only the sender
    /// receives this — peers see no broadcast, since no state change
    /// happened.
    OpError { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Op { op: SyncOp },
}
