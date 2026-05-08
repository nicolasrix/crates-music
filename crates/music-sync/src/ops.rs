//! The op enum exchanged over the sync protocol.
//!
//! Wire format is JSON, internally tagged on `"type"` with snake_case
//! variant names. Adding a new variant is non-breaking; renaming or
//! removing fields IS breaking — this enum is part of the gateway's
//! public API.

use serde::{Deserialize, Serialize};

use music_core::{QueueItemId, TrackId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncOp {
    /// Append a new item to the end of the queue. The client picks
    /// `item_id` (a v7 UUID is the recommended format) so the op is
    /// idempotent on retry.
    Push {
        item_id: QueueItemId,
        track_id: TrackId,
    },
    /// Remove an item by id. No-op if the id is not in the queue.
    Remove { item_id: QueueItemId },
    /// Move an item to `new_index`. The index is into the queue
    /// *after* the item is removed from its current slot, and is
    /// clamped to the valid range.
    Reorder {
        item_id: QueueItemId,
        new_index: usize,
    },
    /// Move (or clear) the playback cursor.
    SetNowPlaying { index: Option<usize> },
    /// Set the playback head position in milliseconds.
    SetPosition { position_ms: u64 },
    /// Set the play/pause flag.
    SetPlaying { is_playing: bool },
    /// Empty the queue and clear the cursor.
    Clear,
}
