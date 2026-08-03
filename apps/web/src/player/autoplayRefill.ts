// Pure helper: decide what an in-flight autoplay refill should actually
// push, once its recommendations finally land.
//
// The subtlety this exists for: a refill outlives the render that started
// it. Between "ask the gateway for 5 tracks" and "the 5 tracks arrive"
// there is a network round-trip plus a `getSong` hydration fan-out, and
// during that window the queue almost always moves — the `applied` frame
// for the very click that triggered the refill is itself a queue change.
//
// The old code handled that by capturing a `cancelled` flag in the effect
// cleanup and discarding the whole result set if anything had changed.
// That threw away good recommendations for a queue that was still
// perfectly valid, and — because the superseding effect run bailed on the
// still-held in-flight lock — nothing retried. Autoplay went quiet until
// some unrelated queue change happened to wake the effect up. Observed
// live: the gateway served 5 tracks twice in eight seconds and the queue
// stayed at one item.
//
// So instead of asking "did anything change?", ask the two questions that
// actually matter:
//
//   1. Is this result set still *about* the right thing? Only a new
//      session invalidates it — those picks were seeded from a queue that
//      no longer exists. A push, a cursor advance or a reorder does not.
//   2. How many does the queue need *now*? Recomputed against the live
//      queue, never the snapshot the request was built from, so a refill
//      that overlaps another one cannot overfill.
//
// Keeping this pure (and out of the effect) is what makes it testable —
// the web app has no React testing library, so the decision logic lives
// where vitest can reach it.

import type { Track } from "../api/types";
import type { QueueItem } from "../sync/types";

/** Why a refill stopped where it did. Drives the caller's cooldown and
 *  whether the effect needs an explicit re-arm. */
export type RefillDisposition =
  /** Placed everything the queue asked for. */
  | "delivered"
  /** The recommender came back with fewer usable tracks than needed —
   *  usually its diversity filter eating candidates. Retrying straight
   *  away with the same seeds would return the same nothing. */
  | "short"
  /** A different session started while we were fetching. Not a failure;
   *  just aimed at a queue that no longer exists. */
  | "stale"
  /** The queue filled up by other means while we were fetching. */
  | "satisfied";

export interface RefillPlanInput {
  /** Session anchor id at the moment the request went out. `undefined`
   *  when the queue has no session (a pushed-together queue). */
  requestSessionId: string | undefined;
  /** The queue as it stands *now*, after the round-trip. */
  liveItems: readonly QueueItem[];
  liveNowPlayingIndex: number | null;
  liveSessionId: string | undefined;
  /** Hydrated recommendations in server-ranked order. */
  tracks: readonly Track[];
  /** How many upcoming tracks the queue is kept topped up to. */
  minUpcoming: number;
}

export interface RefillPlan {
  /** Tracks to push, in order. Never longer than `target`. */
  push: Track[];
  /** How many the live queue turned out to need. */
  target: number;
  disposition: RefillDisposition;
}

const EMPTY = (disposition: RefillDisposition): RefillPlan => ({
  push: [],
  target: 0,
  disposition,
});

export function planRefill(input: RefillPlanInput): RefillPlan {
  const {
    requestSessionId,
    liveItems,
    liveNowPlayingIndex,
    liveSessionId,
    tracks,
    minUpcoming,
  } = input;

  // A session change is the one thing that genuinely invalidates the
  // result set. Note this compares `undefined` to `undefined` happily:
  // a session-less queue stays valid as long as it stays session-less.
  if (liveSessionId !== requestSessionId) return EMPTY("stale");
  // No cursor means nothing is playing, so "upcoming" is undefined and
  // appending would be guesswork. Treat it like a session change.
  if (liveNowPlayingIndex === null) return EMPTY("stale");

  const upcoming = liveItems.length - (liveNowPlayingIndex + 1);
  const target = minUpcoming - upcoming;
  if (target <= 0) return EMPTY("satisfied");

  // Dedup against the live queue, and against earlier picks in this same
  // result set — the gateway excludes what we told it about, but our
  // snapshot of the queue was already stale when we sent it.
  const seen = new Set<string>(liveItems.map((it) => it.track_id));
  const push: Track[] = [];
  for (const t of tracks) {
    if (push.length >= target) break;
    if (seen.has(t.id)) continue;
    seen.add(t.id);
    push.push(t);
  }

  return {
    push,
    target,
    disposition: push.length >= target ? "delivered" : "short",
  };
}

/** True when the effect must re-arm itself explicitly after its cooldown.
 *
 *  `delivered` and `satisfied` leave the queue at threshold, so there is
 *  nothing to retry. `stale` and `short` leave it *under* threshold —
 *  and note that a partial push is not enough to recover on its own:
 *  it does change `queue.items` and re-run the effect, but the in-flight
 *  lock is still held at that moment, so the natural re-trigger is
 *  swallowed. The explicit re-arm after the cooldown is what actually
 *  gets the queue moving again. */
export function needsRearm(plan: RefillPlan): boolean {
  return plan.disposition === "stale" || plan.disposition === "short";
}

/** True when the caller should back off hard before retrying. Only
 *  genuine underdelivery earns it: the same seeds against the same queue
 *  would return the same nothing, so the wait is for the cursor to move
 *  and freshen the seed pool. A stale result is not a failure and gets
 *  the short cooldown. */
export function isUnderdelivery(plan: RefillPlan): boolean {
  return plan.disposition === "short";
}
