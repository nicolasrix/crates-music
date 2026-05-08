//! Top-level playback state: the queue plus the cursor into it.

use serde::{Deserialize, Serialize};

use crate::queue::{Queue, QueueItem};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackState {
    #[serde(default)]
    pub queue: Queue,
    #[serde(default)]
    pub now_playing_index: Option<usize>,
    #[serde(default)]
    pub position_ms: u64,
    #[serde(default)]
    pub is_playing: bool,
}

impl PlaybackState {
    /// The currently-playing item, if any. Returns `None` when there is
    /// no cursor or the cursor is out of bounds — never panics.
    pub fn now_playing(&self) -> Option<&QueueItem> {
        let i = self.now_playing_index?;
        self.queue.items.get(i)
    }
}
