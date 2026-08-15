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
//
// Refill lifecycle — three pieces, and all three are load-bearing:
//
//   • `isRefillingRef` keeps two refills from racing. It is held for a
//     cooldown past the request so the WS echo can land first.
//   • `liveRef` lets the async continuation read the queue as it is when
//     the recommendations arrive, not as it was when they were asked
//     for. `planRefill` decides from that; see `autoplayRefill.ts` for
//     why "did anything change?" is the wrong question.
//   • `retryTick` re-arms the effect. Necessary because the lock means a
//     dep change *during* a refill is swallowed — the superseding effect
//     run returns immediately and schedules nothing. Without the re-arm,
//     any refill that ends up placing nothing leaves the queue parked
//     under threshold until an unrelated change wakes the effect.
//
// `submit()` in SyncContext has no optimistic apply, so local state only
// moves on the server's echo. That is what makes the window wide enough
// to matter: the queue is always a round-trip behind the ops we sent.

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
import { isUnderdelivery, needsRearm, planRefill } from "./autoplayRefill";
import { buildAutoplaySeeds } from "./autoplaySeeds";
import {
  AutoplaySettings,
  loadAutoplaySettings,
  saveAutoplaySettings,
} from "./autoplaySettings";

// Threshold the queue refill targets. The server applies the artist
// cap + dedup, so we ask for exactly `need` candidates per refill
// (the server's internal buffer factor handles cap rejects).
const MIN_UPCOMING = 5;
const STORAGE_KEY = "crates-music.autoplay";

// Slate selection algorithm. We opted into MMR after the λ-sweep at
// `bench-results/lambda-sweep/`; the λ value itself is now a user-tunable
// setting (default 0.8, the swept sweet spot). Hard-cap remains the
// server-side default for clients that don't send a mode.
const DIVERSITY_MODE = "mmr" as const;
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
  /** Record queue items as recommender output. Used by smart shuffle,
   *  which mixes recommendations into a context rather than appending
   *  them — same provenance, different placement, so the feedback
   *  buttons must light up for both. */
  markRecommendations: (itemIds: readonly string[]) => void;
  /** Tethered-drift tuning (leash radius/strength, frontier, MMR λ).
   *  Read by the refill effect and edited from the Settings page. */
  settings: AutoplaySettings;
  setSettings: (s: AutoplaySettings) => void;
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

  // Tethered-drift tuning. Lazy-init from localStorage; persisted on every
  // change so the refill effect (and a reload) always see the latest.
  const [settings, setSettingsState] = useState<AutoplaySettings>(() =>
    loadAutoplaySettings(),
  );
  const setSettings = useCallback((s: AutoplaySettings) => {
    setSettingsState(s);
    saveAutoplaySettings(s);
  }, []);

  const { state, pushTrack } = useSync();
  const { queue, now_playing_index, session_anchor } = state.playback;

  // Lock: true while a refill is in flight. We deliberately leave it
  // set for a short cooldown after the pushes resolve, see comment on
  // REFILL_COOLDOWN_MS.
  const isRefillingRef = useRef(false);

  // Live mirror of the playback slice, read by the async refill after
  // its await. A refill outlives the render that started it, so pushing
  // against the values captured in that render overfills (or, as the old
  // `cancelled` flag did, discards a perfectly good result set). See the
  // header of `autoplayRefill.ts`. Written from an effect rather than
  // during render so the continuation only ever sees committed state.
  const liveRef = useRef({
    items: queue.items,
    nowPlayingIndex: now_playing_index,
    sessionId: session_anchor?.session_id,
  });
  useEffect(() => {
    liveRef.current = {
      items: queue.items,
      nowPlayingIndex: now_playing_index,
      sessionId: session_anchor?.session_id,
    };
  });

  // Explicit re-arm for refills that placed nothing. One that places
  // tracks re-triggers the effect through `queue.items`; one that aborts
  // or comes back empty changes no dep, so without this the queue sits
  // under threshold until something unrelated wakes the effect.
  const [retryTick, bumpRetry] = useState(0);

  // Unmount guard. Deliberately *not* the old per-run `cancelled` flag:
  // a re-render must not discard a refill, but a torn-down provider must.
  const disposedRef = useRef(false);
  useEffect(() => {
    disposedRef.current = false;
    return () => {
      disposedRef.current = true;
    };
  }, []);

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
  const markRecommendations = useCallback((itemIds: readonly string[]) => {
    for (const id of itemIds) recommendedIdsRef.current.add(id);
  }, []);

  useEffect(() => {
    if (!autoplay) return;
    if (now_playing_index === null) return;
    const items = queue.items;
    if (items.length === 0) return;

    const upcomingStart = now_playing_index + 1;
    const upcomingCount = items.length - upcomingStart;
    if (upcomingCount >= MIN_UPCOMING) return;
    if (isRefillingRef.current) return;

    // Tethered-drift seed plan:
    //   boundary: anchor (3x) > user-picked (2x) > scrobble (1x)  → anchorIds
    //   direction: recency-decayed recent tail (incl. algo-added) → frontier
    // The boundary roots the leash; the frontier lets the station travel.
    const { seeds: weightedSeeds, anchorIds } = buildAutoplaySeeds({
      items,
      nowPlayingIndex: now_playing_index,
      sessionAnchor: session_anchor,
      recommendedItemIds: recommendedIdsRef.current,
      frontier: {
        weight: settings.frontierWeight,
        decay: settings.frontierDecay,
        window: settings.frontierWindow,
      },
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

    const need = MIN_UPCOMING - upcomingCount;
    const nowPlayingTrackId = items[now_playing_index]?.track_id;
    const requestSessionId = session_anchor?.session_id;

    isRefillingRef.current = true;
    void (async () => {
      // Assume the worst until the plan says otherwise: a thrown request
      // is indistinguishable from underdelivery here, and "back off, then
      // try again" is the right response to both.
      let rearm = true;
      let backOff = true;
      try {
        const queueContext = {
          queueTrackIds: items.map((it) => it.track_id),
          ...(nowPlayingTrackId ? { nowPlayingTrackId } : {}),
          diversityMode: DIVERSITY_MODE,
          mmrLambda: settings.mmrLambda,
        };
        const { tracks } =
          weightedSeeds.length > 0
            ? await startWeightedStation(weightedSeeds, need, queueContext, requestSessionId, {
                anchorIds,
                tau: settings.leashTau,
                lambda: settings.leashLambda,
              })
            : await startStationFromAny(fallbackCandidates, need, queueContext);
        if (disposedRef.current) {
          rearm = false;
          backOff = false;
          return;
        }
        // Re-decide against the queue as it is *now*, not the snapshot
        // this request was built from — see `autoplayRefill.ts`.
        const live = liveRef.current;
        const plan = planRefill({
          requestSessionId,
          liveItems: live.items,
          liveNowPlayingIndex: live.nowPlayingIndex,
          liveSessionId: live.sessionId,
          tracks,
          minUpcoming: MIN_UPCOMING,
        });
        rearm = needsRearm(plan);
        backOff = isUnderdelivery(plan);
        // Synchronous: every push lands before React can re-render, so
        // the batch is all-or-nothing from any observer's point of view.
        for (const t of plan.push) {
          const newItemId = pushTrack(t);
          recommendedIdsRef.current.add(newItemId);
        }
      } catch (err) {
        // Not fatal — recommender unavailable, no embedded seed, etc.
        // Kept off the UI (a toast on every miss would be noise) but no
        // longer invisible: this catch used to swallow the only evidence
        // that autoplay had stopped refilling.
        console.warn("[autoplay] refill failed:", err);
      } finally {
        // Cooldown to outlast the WS round-trip; see top-of-file note.
        // Release the lock unconditionally — a lock held past its
        // refill is exactly what turns one dropped result set into
        // "autoplay never recovers".
        //
        // Genuine underdelivery → long cooldown. The server's diversity
        // filter ate the candidates; retrying immediately with the same
        // seeds and queue would yield the same nothing. A *stale* result
        // (the session moved under us) is not underdelivery and takes
        // the short cooldown — we want to re-ask for the new session
        // promptly, which is the whole point of the re-arm below.
        const cooldown = backOff
          ? UNDERDELIVERY_COOLDOWN_MS
          : REFILL_COOLDOWN_MS;
        setTimeout(() => {
          isRefillingRef.current = false;
          if (rearm && !disposedRef.current) bumpRetry((n) => n + 1);
        }, cooldown);
      }
    })();
  }, [
    autoplay,
    queue.items,
    now_playing_index,
    session_anchor,
    pushTrack,
    settings,
    retryTick,
  ]);

  return (
    <Ctx.Provider
      value={{
        autoplay,
        setAutoplay,
        isRecommendation,
        markRecommendations,
        settings,
        setSettings,
      }}
    >
      {children}
    </Ctx.Provider>
  );
}

export function useAutoplay(): AutoplayCtx {
  const v = useContext(Ctx);
  if (!v) throw new Error("useAutoplay must be used inside <AutoplayProvider>");
  return v;
}
