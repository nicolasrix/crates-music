//! Server-authoritative playback state and the deterministic op
//! application function. The gateway holds one of these per session
//! (a single user, but conceptually scoped) and applies ops in arrival
//! order. Last-Writer-Wins per field falls out of linear application.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use music_core::{PlaybackState, QueueItem, QueueItemId};

use crate::ops::SyncOp;

/// The full sync state: current playback plus a monotonic op counter.
///
/// `version` is bumped only when [`SyncState::apply`] returns `Ok`. A
/// rejected op leaves `version` unchanged so connected clients can
/// detect "we missed nothing" by checking version monotonicity on the
/// fan-out channel.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncState {
    #[serde(default)]
    pub playback: PlaybackState,
    #[serde(default)]
    pub version: u64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ApplyError {
    #[error("now_playing index {index} is out of bounds (queue length {len})")]
    NowPlayingOutOfBounds { index: usize, len: usize },
}

impl SyncState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply an op. Returns `Err` for hard rejects (currently only
    /// out-of-bounds `SetNowPlaying`). On `Ok`, the version counter
    /// advances by one.
    pub fn apply(&mut self, op: &SyncOp) -> Result<(), ApplyError> {
        match op {
            SyncOp::Push { item_id, track_id } => {
                if self.playback.queue.position_of(item_id).is_none() {
                    self.playback.queue.items.push(QueueItem {
                        item_id: item_id.clone(),
                        track_id: track_id.clone(),
                    });
                }
                // If item_id was already present, this is a retried op:
                // accepted as a no-op. Version still bumps so listeners
                // see a consistent event count.
            }
            SyncOp::Remove { item_id } => self.apply_remove(item_id),
            SyncOp::Reorder { item_id, new_index } => self.apply_reorder(item_id, *new_index),
            SyncOp::SetNowPlaying { index } => {
                if let Some(i) = index {
                    let len = self.playback.queue.items.len();
                    if *i >= len {
                        return Err(ApplyError::NowPlayingOutOfBounds { index: *i, len });
                    }
                }
                self.playback.now_playing_index = *index;
            }
            SyncOp::SetPosition { position_ms } => {
                self.playback.position_ms = *position_ms;
            }
            SyncOp::SetPlaying { is_playing } => {
                self.playback.is_playing = *is_playing;
            }
            SyncOp::Clear => {
                self.playback.queue.items.clear();
                self.playback.now_playing_index = None;
                self.playback.position_ms = 0;
                self.playback.is_playing = false;
            }
        }
        self.version += 1;
        Ok(())
    }

    fn apply_remove(&mut self, item_id: &QueueItemId) {
        let Some(pos) = self.playback.queue.position_of(item_id) else {
            return;
        };
        self.playback.queue.items.remove(pos);
        self.playback.now_playing_index = match self.playback.now_playing_index {
            Some(cursor) if pos < cursor => Some(cursor - 1),
            Some(cursor) => Some(cursor),
            None => None,
        };
        self.normalize_cursor();
    }

    fn apply_reorder(&mut self, item_id: &QueueItemId, new_index: usize) {
        let Some(old) = self.playback.queue.position_of(item_id) else {
            return;
        };
        // Track which item the cursor is on, so we can re-locate it
        // after the move (cursor follows track-id, not numeric index).
        let cursor_item_id = self
            .playback
            .now_playing_index
            .and_then(|i| self.playback.queue.items.get(i))
            .map(|i| i.item_id.clone());

        let item = self.playback.queue.items.remove(old);
        let target = new_index.min(self.playback.queue.items.len());
        self.playback.queue.items.insert(target, item);

        if let Some(id) = cursor_item_id {
            self.playback.now_playing_index = self.playback.queue.position_of(&id);
        }
    }

    fn normalize_cursor(&mut self) {
        if let Some(i) = self.playback.now_playing_index
            && i >= self.playback.queue.items.len()
        {
            self.playback.now_playing_index = None;
        }
    }
}
