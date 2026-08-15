//! Server-authoritative playback state and the deterministic op
//! application function. The gateway holds one of these per session
//! (a single user, but conceptually scoped) and applies ops in arrival
//! order. Last-Writer-Wins per field falls out of linear application.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use music_core::{PlaybackState, QueueItem, QueueItemId, SessionAnchor};

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
    #[error("start_session items must not be empty")]
    StartSessionEmpty,
}

impl SyncState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply an op. Returns `Err` for hard rejects (out-of-bounds
    /// `SetNowPlaying` or `StartSession`, empty `StartSession.items`).
    /// On `Ok`, the version counter advances by one.
    ///
    /// `now_ms` is the server's wall-clock timestamp at apply time.
    /// The state machine itself never calls `SystemTime::now()` —
    /// pushing the clock to the caller keeps `apply` pure and lets
    /// tests replay history with synthetic time.
    pub fn apply(&mut self, op: &SyncOp, now_ms: i64) -> Result<(), ApplyError> {
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
                self.playback.session_anchor = None;
            }
            SyncOp::StartSession {
                items,
                anchor_index,
                session_id,
            } => {
                if items.is_empty() {
                    return Err(ApplyError::StartSessionEmpty);
                }
                if *anchor_index >= items.len() {
                    return Err(ApplyError::NowPlayingOutOfBounds {
                        index: *anchor_index,
                        len: items.len(),
                    });
                }
                let anchor_track = items[*anchor_index].track_id.clone();
                self.playback.queue.items.clone_from(items);
                self.playback.now_playing_index = Some(*anchor_index);
                self.playback.position_ms = 0;
                self.playback.is_playing = true;
                self.playback.session_anchor = Some(SessionAnchor {
                    session_id: session_id.clone(),
                    track_id: anchor_track,
                    started_ms: now_ms,
                });
            }
            SyncOp::ReplaceUpcoming { items } => self.apply_replace_upcoming(items),
            SyncOp::StopSession => {
                self.playback.session_anchor = None;
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

    /// Swap the queue's tail (everything the cursor hasn't reached) for
    /// `items`. History and the current track survive untouched, which is
    /// what makes a live shuffle-mode flip safe: the `<audio>` element on
    /// every client keeps playing the same track at the same position.
    fn apply_replace_upcoming(&mut self, items: &[QueueItem]) {
        // A valid cursor is always in bounds, so `keep` can't exceed the
        // queue length; `truncate` is a no-op in the None case anyway.
        let keep = self.playback.now_playing_index.map_or(0, |i| i + 1);
        self.playback.queue.items.truncate(keep);
        // Seeded from what survived, so `insert` rejects both a collision
        // with history and a repeat inside `items` itself.
        let mut seen: HashSet<QueueItemId> = self
            .playback
            .queue
            .items
            .iter()
            .map(|i| i.item_id.clone())
            .collect();
        let fresh: Vec<QueueItem> = items
            .iter()
            .filter(|i| seen.insert(i.item_id.clone()))
            .cloned()
            .collect();
        self.playback.queue.items.extend(fresh);
    }

    fn normalize_cursor(&mut self) {
        if let Some(i) = self.playback.now_playing_index
            && i >= self.playback.queue.items.len()
        {
            self.playback.now_playing_index = None;
        }
    }
}
