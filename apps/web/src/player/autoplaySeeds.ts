// Pure helper: build the weighted-seed list for autoplay refill.
//
// The provenance-based weights break the recommender's drift loop: by
// weighting the user's anchored track 3x and user-picked items 2x,
// over already-played scrobbles at 1x, and *excluding* items added by
// the recommender itself, the seed pool stays rooted in the user's
// stated intent rather than walking off into whatever the recommender
// suggested most recently.

import type { WeightedSeed } from "../api/recommend";
import type { QueueItem, SessionAnchor } from "../sync/types";

export interface SeedBuildInput {
  /** Current queue, in order. */
  items: readonly QueueItem[];
  /** Index of the now-playing item in `items`, or null if no cursor. */
  nowPlayingIndex: number | null;
  /** Current session anchor (if any). */
  sessionAnchor: SessionAnchor | null;
  /** Item ids that were *pushed by the recommender* (not user-picked).
   *  Used to exclude algo-picked items from the seed pool so the
   *  recommender doesn't reseed from its own output. */
  recommendedItemIds: ReadonlySet<string>;
}

export const WEIGHT_ANCHOR = 3.0;
export const WEIGHT_USER_PICKED = 2.0;
export const WEIGHT_SCROBBLE = 1.0;

/** Build the weighted seed list for an autoplay refill.
 *
 * Rules:
 * - The session-anchor's track gets weight 3 (always, when present).
 * - User-picked queue items (not recommendation-added) at or after
 *   the cursor get weight 2.
 * - User-picked queue items *before* the cursor (already-played
 *   scrobbles within the session) get weight 1.
 * - Recommendation-added items contribute *nothing* — they're the
 *   feedback-loop trap that caused the genre drift this fixes.
 *
 * Duplicates by track_id collapse to the *highest* weight encountered.
 * Empty result when there are no eligible seeds. */
export function buildAutoplaySeeds(input: SeedBuildInput): WeightedSeed[] {
  const { items, nowPlayingIndex, sessionAnchor, recommendedItemIds } = input;
  // track_id → highest weight seen so far
  const byTrack = new Map<string, number>();

  const bump = (trackId: string, weight: number) => {
    const prev = byTrack.get(trackId);
    if (prev === undefined || weight > prev) byTrack.set(trackId, weight);
  };

  if (sessionAnchor) bump(sessionAnchor.track_id, WEIGHT_ANCHOR);

  for (let i = 0; i < items.length; i++) {
    const it = items[i];
    if (!it) continue;
    if (recommendedItemIds.has(it.item_id)) continue; // skip algo-picked
    const isBeforeCursor =
      nowPlayingIndex !== null && i < nowPlayingIndex;
    bump(it.track_id, isBeforeCursor ? WEIGHT_SCROBBLE : WEIGHT_USER_PICKED);
  }

  // Stable order: by descending weight, then by track_id for determinism.
  return Array.from(byTrack.entries())
    .map(([trackId, weight]) => ({ trackId, weight }))
    .sort((a, b) => b.weight - a.weight || a.trackId.localeCompare(b.trackId));
}
