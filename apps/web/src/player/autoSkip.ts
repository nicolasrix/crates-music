// Pure helpers for the player's dislike auto-skip.
//
// When the queue advances *onto* a disliked track, the player walks in the
// advance direction to the first track that isn't disliked. Extracted as
// pure functions (à la evaluateSkip / evaluateScrobble) so the index walk
// and the dislike test are unit-testable without a DOM, an audio element,
// or sync state.

/** Verdict map: entity id → "like" | "dislike". Absence = neutral. The
 *  React-free shape of `RatingMap` from useRatings.ts. */
export type DislikeMap = ReadonlyMap<string, "like" | "dislike">;

/** Album/artist of a track, as known to the client (either may be absent
 *  if the track isn't hydrated yet — see the convergence note below). */
export interface TrackParents {
  albumId?: string;
  artistId?: string;
}

/**
 * Whether a track is excluded from play by a dislike at *any* level: the
 * track itself, its album, or its artist. Mirrors the server-side
 * `disliked_exclusions` union so the player auto-skips exactly what the
 * recommender would never surface.
 *
 * `parents` comes from the client's hydrated metadata for the queue item.
 * If a not-yet-current track isn't hydrated (`parents` undefined / partial),
 * its album/artist dislike can't be detected on the lookahead walk — but the
 * auto-skip effect re-fires on every advance, so once the cursor lands on
 * that track it *is* hydrated and gets skipped then (a harmless extra hop).
 */
export function isDislikedEntity(
  trackId: string | undefined,
  parents: TrackParents | undefined,
  maps: { tracks: DislikeMap; albums: DislikeMap; artists: DislikeMap },
): boolean {
  if (trackId === undefined) return false;
  if (maps.tracks.get(trackId) === "dislike") return true;
  if (parents?.albumId && maps.albums.get(parents.albumId) === "dislike") return true;
  if (parents?.artistId && maps.artists.get(parents.artistId) === "dislike") return true;
  return false;
}

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
