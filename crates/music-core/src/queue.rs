//! Playback queue: an ordered list of items, each pointing at a track.

use serde::{Deserialize, Serialize};

use crate::ids::{QueueItemId, TrackId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueItem {
    pub item_id: QueueItemId,
    pub track_id: TrackId,
    /// Where the item was queued from. Optional on the wire: clients that
    /// predate it (and anything that doesn't know) omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PlaySource>,
}

/// The surface a queue item was started from — the context a play happened
/// in, which the listening log records so a behavioural model can tell a
/// deliberate pick from an autoplay fill.
///
/// `kind` is an open set stored verbatim (`album`, `playlist`, `artist`,
/// `liked`, `search`, `home`, `queue_add`, `autoplay`, `smart_shuffle`, …),
/// so a client can add a surface without a protocol change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaySource {
    pub kind: String,
    /// The entity the surface was showing (album / playlist / artist id),
    /// when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
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
