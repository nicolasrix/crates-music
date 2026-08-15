// PlayModeContext: owns the play mode (in order / shuffle / shuffle with
// recommendations) and everything that follows from it — how a clicked
// list becomes a queue, and what happens to a live queue when the mode
// changes under it.
//
// Why it sits between the sync layer and the pages: starting a context is
// the only moment we know what the *context* was. The server queue is
// just items; once shuffled it has no memory of the album it came from.
// So this provider keeps that list (`contextRef`, mirrored to
// localStorage) and hands it to the planner whenever the mode flips.
//
// Three rules the implementation is arranged around:
//
//   1. **The click must not wait.** `audio.play()` only counts while the
//      user's gesture is live, so a context starts the instant it's
//      clicked, already shuffled. Recommendations for smart shuffle are
//      fetched *after* and folded in with a second op — a queue that
//      gains a few tracks a second later is fine; one that doesn't start
//      playing is not.
//   2. **Only the tail is ours.** Every rewrite goes through
//      `replace_upcoming`, which leaves the playing track, its position
//      and the session anchor untouched. Flipping shuffle mid-song must
//      not restart the song — on any device.
//   3. **Never guess an order we didn't record.** With no context memory
//      (queue started on another device), shuffle still shuffles what's
//      queued, but "in order" declines rather than inventing one.

import {
  createContext,
  ReactNode,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
} from "react";
import { suggestForPlaylist } from "../api/recommend";
import type { Track } from "../api/types";
import { playList } from "../sync/playbackHelpers";
import { useSync } from "../sync/SyncContext";
import { useToast } from "../toast/ToastContext";
import { useAutoplay } from "./AutoplayContext";
import {
  loadPlayMode,
  loadStoredContext,
  nextMode,
  savePlayMode,
  saveStoredContext,
  type PlayMode,
} from "./playMode";
import {
  interleave,
  recommendationSlots,
  replanUpcoming,
  startOrder,
  type PlayContext,
} from "./playModePlan";

// One recommendation after every N context tracks. Spotify's Smart
// Shuffle sits around 1-in-4; denser than that and the list stops
// sounding like the user's own.
const RECS_EVERY = 4;
// Ceiling on a single mix. The queue keeps getting topped up as it
// drains (autoplay) — there's no need to plan an hour ahead, and a
// smaller `top_n` keeps the from-seeds call quick.
const MAX_MIX = 10;
// Seeds sampled server-side from the context. Matches the /v1 default.
const SEED_SAMPLE = 8;

interface PlayModeCtx {
  mode: PlayMode;
  /** Step the player-bar button: in order → shuffle → smart shuffle. */
  cycleMode: () => void;
  /** Set the mode directly and re-plan the live queue to match. */
  setMode: (mode: PlayMode) => void;
  /** Start a list of songs as the current context: orders it per the
   *  mode, replaces the queue, and (in smart shuffle) mixes
   *  recommendations in once they arrive. `startIndex` is the track the
   *  user clicked — it always plays first, whatever the mode.
   *
   *  `modeOverride` starts this context in a specific mode *and* makes
   *  it the mode from here on (that's what a "shuffle playlist" button
   *  means). It's applied inline rather than via `setMode` so the queue
   *  is written once — a `setMode` first would re-plan the queue we're
   *  about to throw away. */
  startContext: (
    tracks: readonly Track[],
    startIndex: number,
    modeOverride?: PlayMode,
  ) => void;
}

const Ctx = createContext<PlayModeCtx | null>(null);

export function PlayModeProvider({ children }: { children: ReactNode }) {
  const [mode, setModeState] = useState<PlayMode>(loadPlayMode);
  const { state, startSession, replaceUpcoming } = useSync();
  const { markRecommendations } = useAutoplay();
  const toast = useToast();

  // The list the current session was started from. A ref, not state:
  // nothing renders from it, and the async mix continuation must read
  // the latest value rather than the one its render captured.
  const contextRef = useRef<PlayContext | null>(loadStoredContext());

  // Live mirror of the queue for the same reason AutoplayContext keeps
  // one: a mix that started two seconds ago must fold into the queue as
  // it is now, not as it was when the request went out.
  const { queue, now_playing_index, session_anchor } = state.playback;
  const liveRef = useRef({
    trackIds: queue.items.map((i) => i.track_id),
    nowPlayingIndex: now_playing_index,
    sessionId: session_anchor?.session_id,
  });
  useEffect(() => {
    liveRef.current = {
      trackIds: queue.items.map((i) => i.track_id),
      nowPlayingIndex: now_playing_index,
      sessionId: session_anchor?.session_id,
    };
  });

  // Supersedes an in-flight mix when the user flips modes again or
  // starts something else — the late result would otherwise splice
  // recommendations into a queue nobody asked to have them in.
  const mixTokenRef = useRef(0);
  const disposedRef = useRef(false);
  useEffect(() => {
    disposedRef.current = false;
    return () => {
      disposedRef.current = true;
    };
  }, []);

  /** Context for the session that's actually playing, or null if the
   *  queue came from somewhere we didn't record (another device, a
   *  reload that predates the memory, a station). */
  const activeContext = useCallback((): PlayContext | null => {
    const ctx = contextRef.current;
    const sessionId = liveRef.current.sessionId;
    if (!ctx || !sessionId || ctx.sessionId !== sessionId) return null;
    return ctx;
  }, []);

  /** Fetch recommendations for a context and fold them into its upcoming
   *  half.
   *
   *  `upcoming` is what we just *submitted*, not what `liveRef` holds:
   *  `submit()` has no optimistic apply, so for one round-trip the local
   *  mirror still describes the pre-shuffle queue. Interleaving into
   *  that would write the old order back and undo the shuffle. The live
   *  mirror is used for one thing only — noticing that the listener
   *  advanced past some of those tracks while the recommender thought. */
  const mixRecommendations = useCallback(
    async (plan: {
      sessionId: string;
      upcoming: readonly string[];
      seeds: readonly string[];
    }) => {
      const token = (mixTokenRef.current += 1);
      const slots = recommendationSlots(plan.upcoming.length, RECS_EVERY, MAX_MIX);
      if (slots === 0 || plan.seeds.length === 0) return;

      let tracks: Track[];
      try {
        const result = await suggestForPlaylist(plan.seeds, {
          topN: slots,
          sampleSize: SEED_SAMPLE,
          excludeIds: plan.upcoming,
        });
        tracks = result.tracks;
      } catch (err) {
        // Recommender down or nothing embedded yet — the shuffle itself
        // already happened, so this degrades to a plain shuffle instead
        // of failing the gesture. Logged, not toasted: one warning per
        // missed mix is noise.
        console.warn("[playmode] recommendation mix failed:", err);
        return;
      }
      // Superseded by a newer start/flip, or the provider went away.
      if (disposedRef.current || token !== mixTokenRef.current) return;
      if (tracks.length === 0) return;

      const live = liveRef.current;
      // Same session in the mirror → it has caught up, so its history is
      // authoritative about what's been played meanwhile. Different → the
      // mirror is still a round-trip behind and describes a queue we've
      // already replaced, so it knows nothing about what we submitted.
      const heard =
        live.sessionId === plan.sessionId
          ? new Set(live.trackIds.slice(0, (live.nowPlayingIndex ?? -1) + 1))
          : new Set<string>();
      const base = plan.upcoming.filter((id) => !heard.has(id));
      if (base.length === 0) return;

      const recIds = tracks.map((t) => t.id);
      const mixed = interleave(base, recIds, RECS_EVERY);
      const itemIds = replaceUpcoming(mixed, tracks);
      // Provenance, so the player bar offers thumbs on a mixed-in track
      // exactly as it does on an autoplay one.
      const recSet = new Set(recIds);
      markRecommendations(itemIds.filter((_, i) => recSet.has(mixed[i]!)));
    },
    [markRecommendations, replaceUpcoming],
  );

  const startContext = useCallback(
    (tracks: readonly Track[], startIndex: number, modeOverride?: PlayMode) => {
      if (tracks.length === 0) return;
      const effective = modeOverride ?? mode;
      if (modeOverride !== undefined && modeOverride !== mode) {
        setModeState(modeOverride);
        savePlayMode(modeOverride);
      }
      const { order, anchorIndex } = startOrder(
        effective,
        tracks.map((t) => t.id),
        startIndex,
      );
      const byId = new Map(tracks.map((t) => [t.id, t]));
      const ordered = order.flatMap((id) => {
        const t = byId.get(id);
        return t ? [t] : [];
      });

      const sessionId = playList({ startSession }, ordered, anchorIndex);
      if (!sessionId) return;
      // What we remember is the caller's *original* order (that's the
      // whole point — `order` is the shuffled result), narrowed to the
      // tracks that actually made it into the queue. Those differ only
      // for an over-cap context like the "all tracks" page, where
      // restoring to tracks that were never queued would resurrect a
      // chunk of the library nobody asked for.
      const queued = new Set(order);
      const contextIds = tracks.map((t) => t.id).filter((id) => queued.has(id));
      contextRef.current = { sessionId, trackIds: contextIds };
      saveStoredContext({ sessionId, trackIds: contextIds });
      mixTokenRef.current += 1;
      if (effective === "smart_shuffle") {
        void mixRecommendations({
          sessionId,
          upcoming: order.slice(anchorIndex + 1),
          seeds: contextIds,
        });
      }
    },
    [mode, mixRecommendations, startSession],
  );

  const setMode = useCallback(
    (next: PlayMode) => {
      setModeState(next);
      savePlayMode(next);
      mixTokenRef.current += 1;

      const live = liveRef.current;
      const ctx = activeContext();
      const plan = replanUpcoming({
        mode: next,
        context: ctx,
        queueTrackIds: live.trackIds,
        nowPlayingIndex: live.nowPlayingIndex,
      });
      if (plan === null) {
        // Nothing to re-plan: an empty queue, or — the case worth saying
        // out loud — a queue whose original order we never recorded, so
        // there's nothing to restore it to. The mode still applies to
        // whatever is started next.
        if (next === "in_order" && live.trackIds.length > 0 && !ctx) {
          toast("in order — from the next thing you play", { variant: "info" });
        }
        return;
      }
      replaceUpcoming(plan);
      if (next === "smart_shuffle" && live.sessionId) {
        void mixRecommendations({
          sessionId: live.sessionId,
          upcoming: plan,
          seeds: ctx ? ctx.trackIds : live.trackIds,
        });
      }
    },
    [activeContext, mixRecommendations, replaceUpcoming, toast],
  );

  const cycleMode = useCallback(() => setMode(nextMode(mode)), [mode, setMode]);

  return (
    <Ctx.Provider value={{ mode, cycleMode, setMode, startContext }}>
      {children}
    </Ctx.Provider>
  );
}

export function usePlayMode(): PlayModeCtx {
  const v = useContext(Ctx);
  if (!v) throw new Error("usePlayMode must be used inside <PlayModeProvider>");
  return v;
}
