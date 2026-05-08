//! Playback queue: an ordered list of items, each pointing at a track.

use serde::{Deserialize, Serialize};

use crate::ids::{QueueItemId, TrackId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueItem {
    pub item_id: QueueItemId,
    pub track_id: TrackId,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queue {
    #[serde(default)]
    pub items: Vec<QueueItem>,
}

impl Queue {
    pub fn position_of(&self, item_id: &QueueItemId) -> Option<usize> {
        self.items.iter().position(|i| &i.item_id == item_id)
    }
}
