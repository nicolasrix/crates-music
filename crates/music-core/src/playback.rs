//! Top-level playback state: the queue plus the cursor into it.

use serde::{Deserialize, Serialize};

use crate::ids::{SessionId, TrackId};
use crate::queue::{Queue, QueueItem};

/// The "intent" tier of playback state: which user-initiated session
/// this queue belongs to. Mechanics (queue contents, cursor, position)
/// can change without touching the anchor — what makes it a *session*
/// is the user's original pick, not what auto-fill has done to the
/// queue since.
///
/// `started_ms` is server-stamped at the moment `StartSession` was
/// applied — clients never set this themselves, to keep clock skew
/// out of recommender feedback queries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAnchor {
    pub session_id: SessionId,
    pub track_id: TrackId,
    pub started_ms: i64,
}

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
    /// The session that "owns" the current queue: a directly-played
    /// track (or an anchored list) starts a session; auto-fill from
    /// the recommender extends the same session. Direct-play of a
    /// different track replaces the anchor (and the queue) atomically
    /// via `SyncOp::StartSession`. Skipped on the wire when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_anchor: Option<SessionAnchor>,
}

impl PlaybackState {
    /// The currently-playing item, if any. Returns `None` when there is
    /// no cursor or the cursor is out of bounds — never panics.
    pub fn now_playing(&self) -> Option<&QueueItem> {
        let i = self.now_playing_index?;
        self.queue.items.get(i)
    }
}
