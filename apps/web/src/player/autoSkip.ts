// Pure helper for the player's dislike auto-skip.
//
// When the queue advances *onto* a disliked track, the player walks in the
// advance direction to the first track that isn't disliked. Extracted as a
// pure function (à la evaluateSkip / evaluateScrobble) so the index walk is
// unit-testable without a DOM, an audio element, or sync state.

/**
 * First index at or beyond `start`, moving by `direction`, whose track is
 * not disliked. Returns `null` if the walk runs off either end of the queue
 * without finding a playable track (the caller stops playback).
 *
 * `start` is the *current* (disliked) index; since it is disliked the walk
 * naturally steps past it. `isDisliked` is called with each candidate index.
 */
export function nextPlayableIndex(
  start: number,
  direction: 1 | -1,
  total: number,
  isDisliked: (index: number) => boolean,
): number | null {
  let i = start;
  while (i >= 0 && i < total) {
    if (!isDisliked(i)) return i;
    i += direction;
  }
  return null;
}
