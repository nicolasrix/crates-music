// Pure helper: build the weighted-seed plan for an autoplay refill.
//
// "Tethered drift" splits the refill into two forces:
//
//   • Boundary (anchor) — the user's stated intent: the session anchor
//     (weight 3), user-picked queue items (weight 2), and already-played
//     user picks / scrobbles (weight 1). Their track ids are returned as
//     `anchorIds`; the gateway leashes every candidate to within a soft
//     cosine radius of the *nearest* of them.
//   • Direction (frontier) — a recency-decayed tail of the most recently
//     played tracks (INCLUDING the recommender's own picks) re-entering
//     the seed pool at a low weight β·decayᵃᵍᵉ. This lets the Σ-similarity
//     aggregation drift toward where the session has been heading instead
//     of starving against the single fixed anchor's neighbourhood.
//
// The frontier is what makes the station *travel*; the leash is what keeps
// it from wandering. Frontier tracks are NOT anchors — they steer
// direction without widening the boundary.

import type { WeightedSeed } from "../api/recommend";
import type { QueueItem, SessionAnchor } from "../sync/types";

export interface FrontierParams {
  /** Base weight β of the most-recent frontier item. 0 ⇒ no frontier
   *  (anchor-only, the pre-drift behaviour). */
  weight: number;
  /** Decay ∈ [0, 1]: item at age `a` (0 = most recent) gets β·decayᵃ. */
  decay: number;
  /** How many recently-played items feed the frontier. */
  window: number;
}

export interface SeedBuildInput {
  /** Current queue, in order. */
  items: readonly QueueItem[];
  /** Index of the now-playing item in `items`, or null if no cursor. */
  nowPlayingIndex: number | null;
  /** Current session anchor (if any). */
  sessionAnchor: SessionAnchor | null;
  /** Item ids that were *pushed by the recommender* (not user-picked).
   *  Algo-picked items are excluded from the *boundary* (anchor) seeds so
   *  the leash never widens to chase the recommender's own output — but
   *  they are still eligible for the *frontier* (direction) seeds. */
  recommendedItemIds: ReadonlySet<string>;
  /** Recency-frontier tuning. Omit (or weight 0) for anchor-only seeding. */
  frontier?: FrontierParams;
}

export const WEIGHT_ANCHOR = 3.0;
export const WEIGHT_USER_PICKED = 2.0;
export const WEIGHT_SCROBBLE = 1.0;

export interface AutoplaySeedPlan {
  /** Weighted seeds for the Σ-similarity aggregation (boundary + frontier),
   *  highest weight first. */
  seeds: WeightedSeed[];
  /** Track ids the gateway should leash candidates to (boundary only). */
  anchorIds: string[];
}

/** Build the weighted seed plan for an autoplay refill.
 *
 * Boundary rules (unchanged from the pre-drift design):
 * - The session-anchor's track gets weight 3 (always, when present).
 * - User-picked queue items at or after the cursor get weight 2.
 * - User-picked queue items before the cursor (scrobbles) get weight 1.
 * - Recommendation-added items contribute no *boundary* weight.
 *
 * Frontier rules (the new travel force):
 * - The last `window` played items (indices ≤ cursor, any provenance) get
 *   weight `β·decayᵃᵍᵉ`, age measured back from the cursor.
 *
 * Duplicate track_ids collapse to the *highest* weight encountered, so a
 * track that is both a user pick and a recent frontier item keeps its
 * boundary weight (and stays an anchor). */
export function buildAutoplaySeeds(input: SeedBuildInput): AutoplaySeedPlan {
  const { items, nowPlayingIndex, sessionAnchor, recommendedItemIds, frontier } =
    input;
  // track_id → highest weight seen so far
  const byTrack = new Map<string, number>();
  // Boundary (anchor) track ids — leash references, never the frontier.
  const anchorIds = new Set<string>();

  const bump = (trackId: string, weight: number) => {
    const prev = byTrack.get(trackId);
    if (prev === undefined || weight > prev) byTrack.set(trackId, weight);
  };

  if (sessionAnchor) {
    bump(sessionAnchor.track_id, WEIGHT_ANCHOR);
    anchorIds.add(sessionAnchor.track_id);
  }

  for (let i = 0; i < items.length; i++) {
    const it = items[i];
    if (!it) continue;
    if (recommendedItemIds.has(it.item_id)) continue; // not a boundary anchor
    const isBeforeCursor = nowPlayingIndex !== null && i < nowPlayingIndex;
    bump(it.track_id, isBeforeCursor ? WEIGHT_SCROBBLE : WEIGHT_USER_PICKED);
    anchorIds.add(it.track_id);
  }

  // Frontier: recency-decayed recently-played items (any provenance). This
  // is the only place algo-added tracks enter the seed pool.
  if (frontier && frontier.weight > 0 && frontier.window > 0 && nowPlayingIndex !== null) {
    const start = Math.max(0, nowPlayingIndex - frontier.window + 1);
    for (let i = nowPlayingIndex; i >= start; i--) {
      const it = items[i];
      if (!it) continue;
      const age = nowPlayingIndex - i;
      const w = frontier.weight * Math.pow(frontier.decay, age);
      if (w > 0) bump(it.track_id, w);
    }
  }

  // Stable order: by descending weight, then by track_id for determinism.
  const seeds = Array.from(byTrack.entries())
    .map(([trackId, weight]) => ({ trackId, weight }))
    .sort((a, b) => b.weight - a.weight || a.trackId.localeCompare(b.trackId));

  return { seeds, anchorIds: Array.from(anchorIds) };
}
