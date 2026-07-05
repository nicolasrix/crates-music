//! Pure transport-queue state machine for interactive playback.
//!
//! Deliberately audio-free: the TUI reducer owns a [`PlayQueue`] and decides
//! *what* should play; the [`crate::Player`] audio thread only knows about the
//! one track it was last handed. Keeping cursor arithmetic here (instead of on
//! the audio thread) makes every edge case — remove-before-cursor, advance
//! past the end, previous-at-zero — unit-testable in CI, where no audio
//! device exists.
//!
//! Named `PlayQueue` to avoid clashing with `music_core::queue::Queue`, the
//! cross-device *sync-protocol* queue. This one is purely local transport
//! state.

use std::time::Duration;

/// One entry in the local play queue — enough metadata to render a queue row
/// and a now-playing bar without re-fetching.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedTrack {
    pub id: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Parent ids, carried so queue-level policy (e.g. dislike auto-skip)
    /// can match album/artist verdicts without re-fetching metadata.
    pub artist_id: Option<String>,
    pub album_id: Option<String>,
    pub duration: Option<Duration>,
}

/// Local play queue with a cursor. All mutation is cursor-preserving where
/// that is well-defined; see individual methods for the edge-case contracts.
#[derive(Debug, Default)]
pub struct PlayQueue {
    items: Vec<QueuedTrack>,
    /// Index of the track that is (or should be) loaded in the player.
    /// `None` = nothing current: empty queue, or playback ran off the end.
    current: Option<usize>,
}

impl PlayQueue {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the whole queue and point the cursor at `start` (clamped to
    /// the last item). Empty `items` clears the cursor.
    pub fn replace(&mut self, items: Vec<QueuedTrack>, start: usize) -> Option<&QueuedTrack> {
        self.items = items;
        self.current = if self.items.is_empty() {
            None
        } else {
            Some(start.min(self.items.len() - 1))
        };
        self.current()
    }

    /// Append to the end. Never moves the cursor — an empty queue stays
    /// cursor-less until the caller explicitly starts playback.
    pub fn enqueue(&mut self, items: Vec<QueuedTrack>) {
        self.items.extend(items);
    }

    /// Insert directly after the current track ("play next"). With no
    /// current track the items go to the front of the queue.
    pub fn enqueue_next(&mut self, items: Vec<QueuedTrack>) {
        let at = self.current.map_or(0, |i| i + 1);
        self.items.splice(at..at, items);
    }

    /// Remove the item at `index` (out-of-range is a no-op).
    ///
    /// Cursor contract: removing *before* the cursor shifts it left so it
    /// keeps pointing at the same track; removing *at* the cursor leaves the
    /// index in place so the next track slides in (clamped to the new last
    /// item; `None` if the queue emptied). The caller decides whether that
    /// warrants loading the new current track.
    pub fn remove(&mut self, index: usize) {
        if index >= self.items.len() {
            return;
        }
        self.items.remove(index);
        self.current = match self.current {
            Some(c) if self.items.is_empty() => {
                let _ = c;
                None
            }
            Some(c) if index < c => Some(c - 1),
            Some(c) => Some(c.min(self.items.len() - 1)),
            None => None,
        };
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.current = None;
    }

    /// Move to the next track (user "next" or natural end-of-track).
    /// Running off the end clears the cursor — the queue is *finished*, and
    /// [`Self::previous`] can still recover the last track.
    pub fn advance(&mut self) -> Option<&QueuedTrack> {
        match self.current {
            Some(i) if i + 1 < self.items.len() => {
                self.current = Some(i + 1);
                self.current()
            }
            _ => {
                self.current = None;
                None
            }
        }
    }

    /// Move to the previous track. At index 0 this stays put (the caller
    /// typically restarts the track). With no cursor (finished queue) it
    /// recovers the last item.
    pub fn previous(&mut self) -> Option<&QueuedTrack> {
        match self.current {
            Some(i) => {
                self.current = Some(i.saturating_sub(1));
                self.current()
            }
            None if !self.items.is_empty() => {
                self.current = Some(self.items.len() - 1);
                self.current()
            }
            None => None,
        }
    }

    /// Point the cursor at `index`. Out-of-range returns `None` and leaves
    /// the cursor unchanged.
    pub fn jump(&mut self, index: usize) -> Option<&QueuedTrack> {
        if index < self.items.len() {
            self.current = Some(index);
            self.current()
        } else {
            None
        }
    }

    #[must_use]
    pub fn current(&self) -> Option<&QueuedTrack> {
        self.current.and_then(|i| self.items.get(i))
    }

    #[must_use]
    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    /// The track after the current one — the prefetch target for
    /// near-gapless handoff.
    #[must_use]
    pub fn next_up(&self) -> Option<&QueuedTrack> {
        self.current.and_then(|i| self.items.get(i + 1))
    }

    #[must_use]
    pub fn items(&self) -> &[QueuedTrack] {
        &self.items
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str) -> QueuedTrack {
        QueuedTrack {
            id: id.to_owned(),
            title: format!("title-{id}"),
            artist: None,
            album: None,
            artist_id: None,
            album_id: None,
            duration: None,
        }
    }

    fn tracks(ids: &[&str]) -> Vec<QueuedTrack> {
        ids.iter().map(|id| track(id)).collect()
    }

    #[test]
    fn replace_sets_cursor_and_clamps() {
        let mut q = PlayQueue::new();
        assert_eq!(q.replace(tracks(&["a", "b", "c"]), 1).unwrap().id, "b");
        // start beyond the end clamps to the last item
        assert_eq!(q.replace(tracks(&["a", "b"]), 9).unwrap().id, "b");
    }

    #[test]
    fn replace_with_empty_clears_cursor() {
        let mut q = PlayQueue::new();
        q.replace(tracks(&["a"]), 0);
        assert!(q.replace(vec![], 0).is_none());
        assert!(q.current().is_none());
        assert!(q.is_empty());
    }

    #[test]
    fn enqueue_appends_without_moving_cursor() {
        let mut q = PlayQueue::new();
        q.replace(tracks(&["a", "b"]), 0);
        q.enqueue(tracks(&["c"]));
        assert_eq!(q.current().unwrap().id, "a");
        assert_eq!(q.len(), 3);
        assert_eq!(q.items()[2].id, "c");
    }

    #[test]
    fn enqueue_on_empty_leaves_cursor_none() {
        let mut q = PlayQueue::new();
        q.enqueue(tracks(&["a"]));
        assert!(q.current().is_none());
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn enqueue_next_inserts_after_current() {
        let mut q = PlayQueue::new();
        q.replace(tracks(&["a", "b"]), 0);
        q.enqueue_next(tracks(&["x", "y"]));
        let ids: Vec<_> = q.items().iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["a", "x", "y", "b"]);
        assert_eq!(q.current().unwrap().id, "a");
    }

    #[test]
    fn enqueue_next_without_current_prepends() {
        let mut q = PlayQueue::new();
        q.enqueue(tracks(&["b"]));
        q.enqueue_next(tracks(&["a"]));
        let ids: Vec<_> = q.items().iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[test]
    fn advance_walks_then_finishes() {
        let mut q = PlayQueue::new();
        q.replace(tracks(&["a", "b"]), 0);
        assert_eq!(q.advance().unwrap().id, "b");
        assert!(q.advance().is_none());
        assert!(q.current().is_none()); // finished, cursor cleared
        // advancing a finished queue stays finished
        assert!(q.advance().is_none());
    }

    #[test]
    fn previous_clamps_at_zero_and_recovers_after_finish() {
        let mut q = PlayQueue::new();
        q.replace(tracks(&["a", "b"]), 1);
        assert_eq!(q.previous().unwrap().id, "a");
        // at zero: stays put (caller restarts the track)
        assert_eq!(q.previous().unwrap().id, "a");
        // run off the end, then previous recovers the last track
        q.jump(1);
        q.advance();
        assert!(q.current().is_none());
        assert_eq!(q.previous().unwrap().id, "b");
    }

    #[test]
    fn previous_on_empty_is_none() {
        let mut q = PlayQueue::new();
        assert!(q.previous().is_none());
    }

    #[test]
    fn jump_in_and_out_of_range() {
        let mut q = PlayQueue::new();
        q.replace(tracks(&["a", "b"]), 0);
        assert_eq!(q.jump(1).unwrap().id, "b");
        assert!(q.jump(5).is_none());
        // failed jump leaves cursor unchanged
        assert_eq!(q.current().unwrap().id, "b");
    }

    #[test]
    fn remove_before_cursor_shifts_it_left() {
        let mut q = PlayQueue::new();
        q.replace(tracks(&["a", "b", "c"]), 2);
        q.remove(0);
        assert_eq!(q.current().unwrap().id, "c");
        assert_eq!(q.current_index(), Some(1));
    }

    #[test]
    fn remove_at_cursor_slides_next_track_in() {
        let mut q = PlayQueue::new();
        q.replace(tracks(&["a", "b", "c"]), 1);
        q.remove(1);
        assert_eq!(q.current().unwrap().id, "c");
        assert_eq!(q.current_index(), Some(1));
    }

    #[test]
    fn remove_last_at_cursor_clamps_back() {
        let mut q = PlayQueue::new();
        q.replace(tracks(&["a", "b"]), 1);
        q.remove(1);
        assert_eq!(q.current().unwrap().id, "a");
    }

    #[test]
    fn remove_only_item_empties_queue() {
        let mut q = PlayQueue::new();
        q.replace(tracks(&["a"]), 0);
        q.remove(0);
        assert!(q.current().is_none());
        assert!(q.is_empty());
    }

    #[test]
    fn remove_after_cursor_keeps_cursor() {
        let mut q = PlayQueue::new();
        q.replace(tracks(&["a", "b", "c"]), 0);
        q.remove(2);
        assert_eq!(q.current().unwrap().id, "a");
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn remove_out_of_range_is_noop() {
        let mut q = PlayQueue::new();
        q.replace(tracks(&["a"]), 0);
        q.remove(7);
        assert_eq!(q.len(), 1);
        assert_eq!(q.current().unwrap().id, "a");
    }

    #[test]
    fn next_up_is_prefetch_target() {
        let mut q = PlayQueue::new();
        q.replace(tracks(&["a", "b"]), 0);
        assert_eq!(q.next_up().unwrap().id, "b");
        q.advance();
        assert!(q.next_up().is_none());
    }

    #[test]
    fn clear_resets_everything() {
        let mut q = PlayQueue::new();
        q.replace(tracks(&["a", "b"]), 1);
        q.clear();
        assert!(q.is_empty());
        assert!(q.current().is_none());
    }
}
