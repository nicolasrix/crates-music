// The ordering maths behind the three play modes. Pure and
// React-free so the interesting cases (flip mid-playback, restore after
// a shuffle, mix recommendations in) are unit-testable without a queue,
// a socket, or a provider tree.
//
// Vocabulary, because two different orderings are in play at once:
//
//   • **context**  — the list the user started from (album, playlist,
//     liked songs, artist top songs…), in its own natural order. Held
//     client-side by PlayModeContext; the server queue has no memory of
//     it once shuffled.
//   • **queue**    — what's actually playing, server-authoritative. Its
//     head is history + the current track; only the tail is ours to
//     rewrite (see the `replace_upcoming` sync op).
//
// Everything here returns *track ids*: the queue's item ids are minted
// at submit time, and the metadata for a track id can always be
// backfilled by SyncContext, so ids are the smallest thing that
// survives a reload.

import { shuffle } from "../utils/shuffle";
import type { PlayMode } from "./playMode";

/** The list a session was started from, remembered so a later flip to
 *  "in order" can put the remaining tracks back the way they were.
 *  Keyed by session id — a queue started on another device (or before a
 *  reload that lost this) simply has no context, and the planner
 *  degrades rather than guessing. */
export interface PlayContext {
  sessionId: string;
  /** The context's own order, not the queue's. */
  trackIds: readonly string[];
}

export interface StartPlan {
  /** Track ids to queue, in play order. */
  order: string[];
  /** Where in `order` playback begins. */
  anchorIndex: number;
}

/** Upper bound on how many tracks one context puts in the queue. Albums
 *  and playlists never come near it; the "all tracks" page is an
 *  infinite scroll over the whole library, and queueing 5 000 items to
 *  play one of them helps nobody. */
export const MAX_CONTEXT = 500;
/** How much of an over-cap context to keep *behind* the clicked track,
 *  so "previous" still works a few presses back. */
const CAPPED_HISTORY = 20;

/** Lay out a fresh session. The clicked track always plays — shuffling
 *  it away from under the user's finger is the one thing every shuffle
 *  implementation agrees not to do — and only what surrounds it moves. */
export function startOrder(
  mode: PlayMode,
  trackIds: readonly string[],
  startIndex: number,
  limit: number = MAX_CONTEXT,
): StartPlan {
  if (trackIds.length === 0) return { order: [], anchorIndex: 0 };
  const start = Math.min(Math.max(startIndex, 0), trackIds.length - 1);

  if (mode !== "in_order") {
    // Shuffled: the pick leads, everything else is reordered behind it.
    const picked = trackIds[start]!;
    const rest = shuffle(trackIds.filter((_, i) => i !== start));
    return { order: [picked, ...rest.slice(0, Math.max(0, limit - 1))], anchorIndex: 0 };
  }

  if (trackIds.length <= limit) {
    // The normal case: the whole list, cursor on the clicked track —
    // the tracks before it stay in the queue as history.
    return { order: [...trackIds], anchorIndex: start };
  }
  const from = Math.max(0, Math.min(start - CAPPED_HISTORY, trackIds.length - limit));
  return { order: trackIds.slice(from, from + limit), anchorIndex: start - from };
}

export interface ReplanInput {
  mode: PlayMode;
  /** The context this session was started from, if we still know it. */
  context: PlayContext | null;
  /** Track ids currently in the queue, in queue order. */
  queueTrackIds: readonly string[];
  /** Cursor into `queueTrackIds`; null when nothing is playing. */
  nowPlayingIndex: number | null;
}

/** New upcoming track ids for a mode flip on a live queue, or `null` when
 *  there's nothing sensible to do (the caller then leaves the queue
 *  alone).
 *
 *  Two shapes, depending on whether the context survived:
 *
 *  - **With context** — the full plan. Shuffle draws from everything in
 *    the context the listener hasn't heard this session; "in order"
 *    restores the context's own order, resuming after the current track.
 *  - **Without context** (queue from another device, or a reload)
 *    — shuffle can still shuffle what's already queued, but there is no
 *    original order to restore, so "in order" returns null. Better a
 *    no-op than a confident wrong answer. */
export function replanUpcoming({
  mode,
  context,
  queueTrackIds,
  nowPlayingIndex,
}: ReplanInput): string[] | null {
  const cursor = nowPlayingIndex ?? -1;
  const heard = new Set(queueTrackIds.slice(0, cursor + 1));

  if (!context) {
    if (mode === "in_order") return null;
    const upcoming = queueTrackIds.slice(cursor + 1);
    return upcoming.length > 0 ? shuffle(upcoming) : null;
  }

  // Anything already behind the cursor stays behind it — including tracks
  // shuffle happened to play early. Re-queueing them on a flip would make
  // the same song come round twice in one pass of an album.
  const remaining = context.trackIds.filter((id) => !heard.has(id));
  if (mode !== "in_order") return shuffle(remaining);

  // Restoring order: resume from where the current track sits in the
  // context, so "shuffle off" mid-album continues the album rather than
  // restarting it. A current track outside the context (a mixed-in
  // recommendation, say) has no position to resume from — fall back to
  // the whole unheard remainder, in context order.
  const current = nowPlayingIndex === null ? undefined : queueTrackIds[nowPlayingIndex];
  const at = current === undefined ? -1 : context.trackIds.indexOf(current);
  if (at < 0) return remaining;
  const after = context.trackIds.slice(at + 1).filter((id) => !heard.has(id));
  return after;
}

/** Spread `extras` through `base`, one after every `every` base tracks.
 *  Used to fold recommendations into a shuffled context — the resulting
 *  queue reads as the user's own list with the occasional stranger, not
 *  as two lists stapled together. Extras left over after `base` runs out
 *  are appended. */
export function interleave(
  base: readonly string[],
  extras: readonly string[],
  every: number,
): string[] {
  if (extras.length === 0) return [...base];
  const step = Math.max(1, Math.floor(every));
  const out: string[] = [];
  let next = 0;
  base.forEach((id, i) => {
    out.push(id);
    if ((i + 1) % step === 0 && next < extras.length) {
      out.push(extras[next]!);
      next += 1;
    }
  });
  out.push(...extras.slice(next));
  return out;
}

/** How many recommendations to ask for when mixing into `upcomingCount`
 *  tracks: one per `every`, never more than `max`, and never any at all
 *  for a queue too short to hide them in. */
export function recommendationSlots(
  upcomingCount: number,
  every: number,
  max: number,
): number {
  const step = Math.max(1, Math.floor(every));
  return Math.max(0, Math.min(max, Math.floor(upcomingCount / step)));
}
