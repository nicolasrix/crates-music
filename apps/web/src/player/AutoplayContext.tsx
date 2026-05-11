// AutoplayContext: owns the "autoplay" toggle and the queue-refill
// effect. When autoplay is on, the effect tops up the upcoming queue
// to MIN_UPCOMING tracks by pulling recommendations from the gateway's
// /v1/recommend/from-any endpoint.
//
// Why a separate context (rather than folding into PlayerContext):
// autoplay is a recommendation-policy concern, not a playback concern,
// and decoupling makes the file diffs cleaner. Both PlayerBar and the
// refill effect read the same `autoplay` flag through this hook.
//
// Persistence: the flag is stored in localStorage so it survives
// reloads. SSR is not in play (this is a Vite SPA) so the lazy
// initializer reading localStorage on first render is safe.
//
// Diversity filtering (artist cap, cross-edition title dedup) used to
// live here. It now runs server-side: every refill posts the current
// queue snapshot as `queue_context` and the gateway returns results
// that already honor the cap. We only retain a tiny exact-id dedup
// against the local queue as a defense against WS-fanout lag (the
// optimistic local push lands a beat before the snapshot reaches
// other devices).

import {
  createContext,
  ReactNode,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
} from "react";
import { startStationFromAny, startWeightedStation } from "../api/recommend";
import { useSync } from "../sync/SyncContext";
import { buildAutoplaySeeds } from "./autoplaySeeds";

// Threshold the queue refill targets. The server applies the artist
// cap + dedup, so we ask for exactly `need` candidates per refill
// (the server's internal buffer factor handles cap rejects).
const MIN_UPCOMING = 5;
const STORAGE_KEY = "crates-music.autoplay";

// Slate selection knobs. We opted into MMR after the λ-sweep at
// `bench-results/lambda-sweep/` showed λ=0.8 is the only point on the
// curve that doesn't trade off admit_mean for the worst-case-tax win.
// Hard-cap is still available server-side as a fallback if a user-level
// override ever lands; for now the value is hard-coded to the swept
// default.
const DIVERSITY_MODE = "mmr" as const;
const MMR_LAMBDA = 0.8;
// Hold the in-flight lock for a beat after pushing so the gateway WS
// round-trip can land before the effect re-evaluates. Without this the
// effect can re-fire while the new items haven't shown up in local
// state yet, double-pushing recommendations.
const REFILL_COOLDOWN_MS = 1500;
// When the recommender genuinely cannot supply more variety from the
// current seeds (everything similar to the seed is already in the
// queue), bumping the cooldown prevents a busy loop. The next natural
// cursor advance will change the seed pool and unblock progress.
const UNDERDELIVERY_COOLDOWN_MS = 30_000;

interface AutoplayCtx {
  autoplay: boolean;
  setAutoplay: (v: boolean) => void;
  /** True when the given queue item was pushed by the autoplay refill
   *  (i.e. came from the recommender). User-picked tracks return false.
   *  Provenance lives only in memory: a page reload resets it, which
   *  is fine — recommendation feedback is moment-in-time signal, and
   *  conflating "I added this manually" with "it came from a rec" is
   *  worse than the buttons going dim after a reload. */
  isRecommendation: (itemId: string | undefined) => boolean;
}

const Ctx = createContext<AutoplayCtx | null>(null);

export function AutoplayProvider({ children }: { children: ReactNode }) {
  const [autoplay, setAutoplayState] = useState<boolean>(() => {
    try {
      return localStorage.getItem(STORAGE_KEY) === "1";
    } catch {
      return false;
    }
  });
  const setAutoplay = useCallback((v: boolean) => {
    setAutoplayState(v);
    try {
      localStorage.setItem(STORAGE_KEY, v ? "1" : "0");
    } catch {
      /* localStorage may be unavailable (private mode); ignore */
    }
  }, []);

  const { state, pushTrack } = useSync();
  const { queue, now_playing_index, session_anchor } = state.playback;

  // Lock: true while a refill is in flight. We deliberately leave it
  // set for a short cooldown after the pushes resolve, see comment on
  // REFILL_COOLDOWN_MS.
  const isRefillingRef = useRef(false);

  // Provenance set: item_ids that the refill effect has pushed. The
  // member check is the "is this a recommendation?" query.
  //
  // We never explicitly remove ids from this set — when a queue item
  // is dropped (clear, remove op, etc.), its id is simply garbage
  // from then on; the lookup against the set is point-in-time-correct
  // and bounded by the queue length anyway. Leaking a few thousand
  // strings over a long session is cheaper than tracking removals
  // through the sync layer.
  const recommendedIdsRef = useRef<Set<string>>(new Set());
  const isRecommendation = useCallback(
    (itemId: string | undefined) =>
      itemId !== undefined && recommendedIdsRef.current.has(itemId),
    [],
  );

  useEffect(() => {
    if (!autoplay) return;
    if (now_playing_index === null) return;
    const items = queue.items;
    if (items.length === 0) return;

    const upcomingStart = now_playing_index + 1;
    const upcomingCount = items.length - upcomingStart;
    if (upcomingCount >= MIN_UPCOMING) return;
    if (isRefillingRef.current) return;

    // Weighted seed list rooted in the user's intent:
    //   anchor (3x) > user-picked (2x) > scrobble (1x) > algo-added (skip)
    // The skip on algo-added items is the key fix for the drift loop
    // that previously had the seed pool walking off into whatever the
    // recommender had last suggested.
    const weightedSeeds = buildAutoplaySeeds({
      items,
      nowPlayingIndex: now_playing_index,
      sessionAnchor: session_anchor,
      recommendedItemIds: recommendedIdsRef.current,
    });
    // Fallback to the legacy single-seed-first-wins path when we have
    // *no* weighted seeds at all — happens when the session anchor
    // hasn't propagated yet and every queue item is algo-picked.
    const fallbackCandidates: string[] = [];
    if (weightedSeeds.length === 0) {
      for (let j = items.length - 1; j >= 0; j--) {
        const id = items[j]?.track_id;
        if (id) fallbackCandidates.push(id);
      }
      if (fallbackCandidates.length === 0) return;
    }

    const queuedIds = new Set<string>(items.map((it) => it.track_id));
    const need = MIN_UPCOMING - upcomingCount;
    const nowPlayingTrackId = items[now_playing_index]?.track_id;

    isRefillingRef.current = true;
    let cancelled = false;
    void (async () => {
      let added = 0;
      try {
        const queueContext = {
          queueTrackIds: items.map((it) => it.track_id),
          ...(nowPlayingTrackId ? { nowPlayingTrackId } : {}),
          diversityMode: DIVERSITY_MODE,
          mmrLambda: MMR_LAMBDA,
        };
        const sessionId = session_anchor?.session_id;
        const { tracks } =
          weightedSeeds.length > 0
            ? await startWeightedStation(weightedSeeds, need, queueContext, sessionId)
            : await startStationFromAny(fallbackCandidates, need, queueContext);
        if (cancelled) return;
        for (const t of tracks) {
          if (added >= need) break;
          // Defense against WS lag: the optimistic local push has
          // already updated `items` but the server's view (and so its
          // exclusion set) might be one snapshot behind. A duplicate
          // that slips through here would be a re-add of a track we
          // just played; cheap to guard against.
          if (queuedIds.has(t.id)) continue;
          const newItemId = pushTrack(t);
          recommendedIdsRef.current.add(newItemId);
          queuedIds.add(t.id);
          added++;
        }
      } catch {
        // Silent — recommender unavailable, no embedded seed, etc.
        // The user-visible effect is "the queue stays under threshold",
        // which is the same fallback behaviour a non-autoplay queue
        // would have anyway. Surfacing this as a toast on every miss
        // would be noisy.
      } finally {
        // Cooldown to outlast the WS round-trip; see top-of-file note.
        // Release the lock unconditionally — `cancelled` is about not
        // pushing stale tracks, not about lock hygiene. Gating release
        // on `cancelled` would deadlock the lock on any non-trivial
        // refill, since cleanup fires on every queue.items broadcast
        // (5+ times during a single refill) and `cancelled` would
        // already be true by the time this timeout runs.
        //
        // Underdelivery → bump the cooldown. Means the server's
        // diversity filter ate most candidates; retrying immediately
        // with the same seeds + queue would yield the same results.
        // Hold off until the cursor moves and the seed pool freshens.
        const cooldown =
          added < need ? UNDERDELIVERY_COOLDOWN_MS : REFILL_COOLDOWN_MS;
        setTimeout(() => {
          isRefillingRef.current = false;
        }, cooldown);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [autoplay, queue.items, now_playing_index, session_anchor, pushTrack]);

  return (
    <Ctx.Provider value={{ autoplay, setAutoplay, isRecommendation }}>
      {children}
    </Ctx.Provider>
  );
}

export function useAutoplay(): AutoplayCtx {
  const v = useContext(Ctx);
  if (!v) throw new Error("useAutoplay must be used inside <AutoplayProvider>");
  return v;
}
