// Audio shell: holds the single <audio> element and reflects the
// authoritative sync state into it. State (queue, cursor, play/pause)
// lives in SyncContext — this layer is just glue between that data and
// the browser's audio API.
//
// On track end we submit a SetNowPlaying op for the next index (or
// SetPlaying(false) at the end of the queue). The eventual broadcast
// flips local state and this effect picks up the change.

import {
  createContext,
  ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { coverArtUrl, scrobble, streamUrl } from "../api/client";
import { postEvents } from "../api/events";
import { markEvent } from "../rum";
import { useSync } from "../sync/SyncContext";
import type { Track } from "../api/types";
import { evaluateScrobble, type ScrobbleState } from "./scrobble";
import { evaluateSkip } from "./skip";
import { isDislikedEntity, nextPlayableIndex } from "./autoSkip";
import { useRatingsMaps } from "./useRatings";

interface PlayerCtx {
  /** The currently-playing item's metadata, if known. */
  nowPlaying: Track | null;
  togglePlay: () => void;
  next: () => void;
  prev: () => void;
  hasNext: boolean;
  hasPrev: boolean;
  isPlaying: boolean;
  queueLength: number;
  /** Direct handle to the underlying <audio> element. Exposed so the
   *  scrubber can read currentTime / duration without us re-broadcasting
   *  every timeupdate through React state (60fps render storm otherwise). */
  audio: HTMLAudioElement | null;
  /** Synchronously load + play a track. Intended to be called from a
   *  click handler *before* the matching sync ops are submitted: the
   *  WS roundtrip would lose the user gesture and the browser's
   *  autoplay policy can refuse the deferred play(). Submitting the
   *  sync ops afterwards just confirms what we've already started. */
  primePlayback: (track: Track) => void;
}

const Ctx = createContext<PlayerCtx | null>(null);

export function PlayerProvider({ children }: { children: ReactNode }) {
  const { state, trackMeta, submit } = useSync();
  const { queue, now_playing_index, is_playing, session_anchor } = state.playback;
  // Lazy-construct on first render (not in a useEffect) so the element is
  // available to consumers — including the Scrubber that reads
  // currentTime/duration off it — from the very first render. The lazy
  // initializer in useState only runs once.
  const [audio] = useState<HTMLAudioElement>(() => {
    const a = new Audio();
    a.preload = "auto";
    return a;
  });
  const audioRef = useRef<HTMLAudioElement>(audio);

  // Active recommend-session id, read through a ref so the skip emitter
  // (stable `[]` deps, called synchronously at gesture sites) can stamp the
  // *current* session without re-binding. Stamping it lets the gateway's
  // provenance log join a served track to its skip outcome session-scoped —
  // without it, skips land session-null and can't be attributed.
  const sessionIdRef = useRef<string | undefined>(session_anchor?.session_id);
  useEffect(() => {
    sessionIdRef.current = session_anchor?.session_id;
  }, [session_anchor]);

  const currentItem = now_playing_index !== null ? queue.items[now_playing_index] : undefined;
  const currentTrackId = currentItem?.track_id;
  const nowPlaying: Track | null =
    currentTrackId !== undefined ? trackMeta.get(currentTrackId) ?? null : null;

  // --- Dislike auto-skip --------------------------------------------------
  //
  // When the queue *advances onto* a disliked track (album play, next,
  // natural end, autoplay refill — all of which funnel through
  // now_playing_index → currentTrackId), skip past it to the next playable
  // track in the advance direction. "Disliked" spans the track itself *or*
  // its album *or* its artist (mirrors the server's exclusion union). A
  // *direct* single-track click is an override: primePlayback records the
  // primed id in directPlayRef and the effect lets it play even if disliked.
  const ratings = useRatingsMaps();
  // The track the user explicitly chose to play — exempt from auto-skip.
  const directPlayRef = useRef<string | null>(null);
  // The disliked track we're actively skipping past. Dedups the WS echo
  // (the effect re-fires on the same id) and signals the src-set effect to
  // not bother loading audio for a track we're leaving immediately.
  const pendingSkipRef = useRef<string | null>(null);
  // Advance direction: +1 normally, -1 only while stepping backward
  // (prev / media previoustrack). Reset to +1 by forward moves.
  const advanceDirRef = useRef<1 | -1>(1);
  // The last track id this effect classified. Auto-skip fires only when the
  // *current* track id changes (a genuine advance onto a new track) — not
  // when the effect re-runs because `ratings` changed. That distinction is
  // what lets you dislike the song that's playing right now without it being
  // yanked out from under you.
  const lastClassifiedRef = useRef<string | null>(null);

  // Declared *before* the src-set effect below so, on a track change, this
  // runs first: it can set pendingSkipRef and redirect the cursor before the
  // src-set effect would otherwise load the disliked track's audio.
  useEffect(() => {
    if (!currentTrackId) return;
    // Fail open while ratings are unknown — never skip a track we can't
    // yet classify. (Ref left untouched so a genuine advance pending here is
    // still evaluated once ratings load.)
    if (ratings === undefined) return;
    // Did the queue just advance onto a *new* track? Only then do we consider
    // auto-skipping. A re-run with the same current track (e.g. the user just
    // disliked the song that's playing) must leave playback alone — the
    // dislike still removes it from recommendations and skips it the next time
    // the queue would land on it.
    const advanced = currentTrackId !== lastClassifiedRef.current;
    lastClassifiedRef.current = currentTrackId;
    // Direct single-track click overrides the skip (consume the marker).
    if (directPlayRef.current === currentTrackId) {
      directPlayRef.current = null;
      pendingSkipRef.current = null;
      return;
    }
    // In-place dislike of the currently-playing track: don't yank it.
    if (!advanced) return;
    // Disliked at the track, album, or artist level. Album/artist need the
    // queue item's hydrated metadata (album_id / artist_id), which we read
    // from the sync layer's trackMeta map.
    const disliked = (id: string | undefined) =>
      isDislikedEntity(id, id !== undefined ? trackMeta.get(id) : undefined, ratings);
    if (!disliked(currentTrackId)) {
      pendingSkipRef.current = null;
      return;
    }
    // Already skipping this exact track — the WS echo re-fired the effect.
    if (pendingSkipRef.current === currentTrackId) return;
    pendingSkipRef.current = currentTrackId;
    const i = now_playing_index;
    if (i === null) return;
    const target = nextPlayableIndex(i, advanceDirRef.current, queue.items.length, (idx) =>
      disliked(queue.items[idx]?.track_id),
    );
    if (target !== null) {
      submit({ type: "set_now_playing", index: target });
    } else {
      // No playable track in this direction — stop rather than sit on a
      // disliked one.
      submit({ type: "set_playing", is_playing: false });
    }
  }, [currentTrackId, ratings, trackMeta, now_playing_index, queue.items, submit]);

  // Swap src when the now-playing track changes. The track id (not the
  // index) is the right dependency: reordering the queue under the
  // cursor is a no-op for playback.
  //
  // We capture the wall clock at src-set, then emit `playback.start`
  // on the *first* `playing` event for this track. That latency
  // (request → first decoded sample) is the one users feel. We don't
  // use `loadedmetadata` because metadata can arrive long before the
  // browser actually starts decoding.
  const startTsRef = useRef<number | null>(null);
  // Per-track scrobble state. Reset on every track change so the
  // now_playing/submission flags follow the cursor. evaluateScrobble
  // lives in scrobble.ts and is purely a function of these four numbers.
  const scrobbleStateRef = useRef<ScrobbleState>({
    trackDurationMs: 0,
    elapsedMs: 0,
    hasEmittedNowPlaying: false,
    hasEmittedSubmission: false,
  });
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    // We're skipping past this (disliked) track — the auto-skip effect has
    // already redirected the cursor. Don't load or play its audio; the next
    // track-change frame will set src for the track we land on.
    if (pendingSkipRef.current === currentTrackId) return;
    scrobbleStateRef.current = {
      trackDurationMs: 0,
      elapsedMs: 0,
      hasEmittedNowPlaying: false,
      hasEmittedSubmission: false,
    };
    if (!currentTrackId) {
      audio.pause();
      return;
    }
    audio.src = streamUrl(currentTrackId);
    startTsRef.current = performance.now();
    if (is_playing) void audio.play().catch(() => {});
  }, [currentTrackId]); // eslint-disable-line react-hooks/exhaustive-deps

  // One-shot `playing` listener per src change emits the latency mark
  // and fires the now_playing scrobble. Both are gated by their own
  // ref-state and so are safe against duplicate `playing` events
  // (which fire on every resume after a pause).
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio || !currentTrackId) return;
    const onPlaying = () => {
      const start = startTsRef.current;
      if (start !== null) {
        const elapsed = performance.now() - start;
        startTsRef.current = null; // one-shot — don't double-emit on resume
        markEvent("playback.start", {
          value_ms: elapsed,
          fields: { track_id: currentTrackId },
        });
      }
      // Now-playing scrobble. We pull duration off the audio element
      // here rather than the timeupdate handler because the duration
      // may not be known on a `play` event (Chrome) yet is reliably
      // populated by the time the first `playing` event fires.
      const durMs = Number.isFinite(audio.duration) ? audio.duration * 1000 : 0;
      const next: ScrobbleState = {
        ...scrobbleStateRef.current,
        trackDurationMs: durMs,
        elapsedMs: audio.currentTime * 1000,
      };
      const decision = evaluateScrobble(next);
      if (decision === "now_playing") {
        scrobbleStateRef.current = { ...next, hasEmittedNowPlaying: true };
        void scrobble(currentTrackId, false).catch(() => {});
      } else {
        scrobbleStateRef.current = next;
      }
    };
    audio.addEventListener("playing", onPlaying);
    return () => audio.removeEventListener("playing", onPlaying);
  }, [currentTrackId]);

  // Submission scrobble: re-evaluate on every timeupdate (fires ~4 Hz
  // in modern browsers — cheap). Captures duration off the element
  // each tick to handle late-arriving metadata changes (rare but real
  // on some HLS-like sources). The pure decision function gates the
  // network call, so this is a no-op once submission has fired.
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio || !currentTrackId) return;
    const onTimeUpdate = () => {
      const durMs = Number.isFinite(audio.duration) ? audio.duration * 1000 : 0;
      const next: ScrobbleState = {
        ...scrobbleStateRef.current,
        trackDurationMs: durMs,
        elapsedMs: audio.currentTime * 1000,
      };
      const decision = evaluateScrobble(next);
      if (decision === "submission") {
        scrobbleStateRef.current = { ...next, hasEmittedSubmission: true };
        void scrobble(currentTrackId, true).catch(() => {});
      } else if (decision === "now_playing") {
        // Rare path: `playing` never fired but timeupdates already are
        // (some autoplay-resume edge cases). Send the now_playing here
        // so the gateway sees a heartbeat for this track.
        scrobbleStateRef.current = { ...next, hasEmittedNowPlaying: true };
        void scrobble(currentTrackId, false).catch(() => {});
      } else {
        scrobbleStateRef.current = next;
      }
    };
    audio.addEventListener("timeupdate", onTimeUpdate);
    return () => audio.removeEventListener("timeupdate", onTimeUpdate);
  }, [currentTrackId]);

  // Skip signal for the recommender. A *manual* track change (next/prev,
  // picking another track, media-key next/prev) abandons the outgoing
  // track; we report how far the user got so the gateway can fold it into
  // per-track preference affinity (early skip = strong "not now", late skip
  // = near-zero penalty). Called synchronously at each gesture site so it
  // reads the audio element while it still holds the *outgoing* track —
  // primePlayback clobbers `src` synchronously, so an effect-cleanup read
  // would see the new track instead. The natural end-of-track auto-advance
  // does not route through these gestures, so it's correctly excluded.
  // evaluateSkip (skip.ts) is the pure gate; fire-and-forget on the POST.
  const maybeEmitSkip = useCallback((trackId: string | undefined) => {
    const audio = audioRef.current;
    if (!audio || !trackId) return;
    const decision = evaluateSkip({
      trackDurationMs: Number.isFinite(audio.duration) ? audio.duration * 1000 : 0,
      playedMs: audio.currentTime * 1000,
      endedNaturally: audio.ended,
    });
    if (!decision.emit) return;
    void postEvents([
      {
        event_type: "skip",
        track_id: trackId,
        occurred_at: Date.now(),
        // Omit (don't send `undefined`) when there's no active session.
        ...(sessionIdRef.current ? { session_id: sessionIdRef.current } : {}),
        metadata: { played_ms: decision.playedMs },
      },
    ]).catch(() => {});
  }, []);

  // Reflect the play/pause flag.
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    if (is_playing) void audio.play().catch(() => {});
    else audio.pause();
  }, [is_playing]);

  // External play/pause sync. Headphone media keys, OS media controls,
  // and the browser's tab-mute affordance all flip the <audio> element
  // directly without going through our React state. Without this listener
  // the element pauses but `is_playing` in sync state stays true, leaving
  // the play/pause icon and UI lying to the user.
  //
  // Submit only when the audio state diverges from the sync state, so the
  // *internal* play/pause we trigger from `is_playing` doesn't bounce back
  // into a duplicate op (would still be a no-op via dedup, but cleaner).
  // We read the latest is_playing through a ref so the listener doesn't
  // need to re-bind on every flip.
  const isPlayingRef = useRef(is_playing);
  useEffect(() => {
    isPlayingRef.current = is_playing;
  }, [is_playing]);
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    const onPlay = () => {
      if (!isPlayingRef.current) submit({ type: "set_playing", is_playing: true });
    };
    const onPause = () => {
      // Ignore pauses that fire as a side-effect of `ended` — the auto-
      // advance handler will flip is_playing if we hit the queue tail, and
      // we don't want to race it here.
      if (audio.ended) return;
      if (isPlayingRef.current) submit({ type: "set_playing", is_playing: false });
    };
    audio.addEventListener("play", onPlay);
    audio.addEventListener("pause", onPause);
    return () => {
      audio.removeEventListener("play", onPlay);
      audio.removeEventListener("pause", onPause);
    };
  }, [submit]);

  // Media Session API: tells the OS / lock screen / headphone display
  // what's playing, and routes media-key gestures through our state
  // machine instead of the raw audio element. Setting an action handler
  // also makes Chrome show the album art in the system-tray controls,
  // which is the user-visible payoff beyond the play/pause sync above.
  useEffect(() => {
    if (typeof navigator === "undefined" || !navigator.mediaSession) return;
    const ms = navigator.mediaSession;
    if (nowPlaying) {
      const artUrl = nowPlaying.coverArt
        ? coverArtUrl(nowPlaying.coverArt, 512, nowPlaying.album ?? nowPlaying.title)
        : null;
      ms.metadata = new MediaMetadata({
        title: nowPlaying.title,
        artist: nowPlaying.artist ?? "",
        album: nowPlaying.album ?? "",
        artwork: artUrl
          ? [{ src: artUrl, sizes: "512x512", type: "image/jpeg" }]
          : [],
      });
    } else {
      ms.metadata = null;
    }
  }, [nowPlaying]);

  useEffect(() => {
    if (typeof navigator === "undefined" || !navigator.mediaSession) return;
    navigator.mediaSession.playbackState = is_playing ? "playing" : "paused";
  }, [is_playing]);

  // Action handlers: route OS-level media controls (headphone buttons,
  // keyboard media keys, system tray) through the same submit() pipeline
  // as the in-app buttons. We re-bind whenever the queue boundary changes
  // so prev/next become unavailable at the ends.
  useEffect(() => {
    if (typeof navigator === "undefined" || !navigator.mediaSession) return;
    const ms = navigator.mediaSession;
    const i = now_playing_index;
    const total = queue.items.length;
    const leaving = i !== null ? queue.items[i]?.track_id : undefined;
    ms.setActionHandler("play", () => submit({ type: "set_playing", is_playing: true }));
    ms.setActionHandler("pause", () => submit({ type: "set_playing", is_playing: false }));
    ms.setActionHandler("nexttrack", () => {
      if (i !== null && i + 1 < total) {
        advanceDirRef.current = 1;
        maybeEmitSkip(leaving);
        submit({ type: "set_now_playing", index: i + 1 });
      }
    });
    ms.setActionHandler("previoustrack", () => {
      if (i !== null && i > 0) {
        advanceDirRef.current = -1;
        maybeEmitSkip(leaving);
        submit({ type: "set_now_playing", index: i - 1 });
      }
    });
    ms.setActionHandler("seekto", (details) => {
      const a = audioRef.current;
      if (!a || details.seekTime === undefined) return;
      a.currentTime = details.seekTime;
    });
    return () => {
      ms.setActionHandler("play", null);
      ms.setActionHandler("pause", null);
      ms.setActionHandler("nexttrack", null);
      ms.setActionHandler("previoustrack", null);
      ms.setActionHandler("seekto", null);
    };
  }, [now_playing_index, queue.items, submit, maybeEmitSkip]);

  // Auto-advance on end. Submitting an op rather than mutating local
  // state keeps the gateway authoritative.
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    const handler = () => {
      const i = now_playing_index;
      if (i === null) return;
      advanceDirRef.current = 1;
      const nextIndex = i + 1;
      if (nextIndex < queue.items.length) {
        submit({ type: "set_now_playing", index: nextIndex });
      } else {
        submit({ type: "set_playing", is_playing: false });
      }
    };
    audio.addEventListener("ended", handler);
    return () => audio.removeEventListener("ended", handler);
  }, [now_playing_index, queue.items.length, submit]);

  const value = useMemo<PlayerCtx>(() => {
    const i = now_playing_index;
    return {
      nowPlaying,
      isPlaying: is_playing,
      queueLength: queue.items.length,
      hasNext: i !== null && i + 1 < queue.items.length,
      hasPrev: i !== null && i > 0,
      audio,
      togglePlay: () => submit({ type: "set_playing", is_playing: !is_playing }),
      next: () => {
        if (i !== null && i + 1 < queue.items.length) {
          advanceDirRef.current = 1;
          maybeEmitSkip(currentTrackId);
          submit({ type: "set_now_playing", index: i + 1 });
        }
      },
      prev: () => {
        if (i !== null && i > 0) {
          advanceDirRef.current = -1;
          maybeEmitSkip(currentTrackId);
          submit({ type: "set_now_playing", index: i - 1 });
        }
      },
      primePlayback: (track) => {
        const a = audioRef.current;
        if (!a) return;
        // A direct click on this track is an override: mark it exempt from
        // dislike auto-skip so it plays even if disliked. Forward is the
        // default advance direction for anything that follows it.
        directPlayRef.current = track.id;
        advanceDirRef.current = 1;
        // Picking a different track abandons the current one — report the
        // skip *before* clobbering src below (which resets currentTime).
        // Restarting the same track is not a skip.
        if (currentTrackId && currentTrackId !== track.id) maybeEmitSkip(currentTrackId);
        // Set src and play() *now*, while still inside the click handler's
        // synchronous user-gesture window. The track-change effect would
        // otherwise duplicate this work later (when set_now_playing's
        // applied frame arrives), but by then the gesture is gone.
        // Setting the same src twice is cheap — the browser dedups.
        a.src = streamUrl(track.id);
        startTsRef.current = performance.now();
        void a.play().catch(() => {});
      },
    };
  }, [
    nowPlaying,
    is_playing,
    queue.items.length,
    now_playing_index,
    submit,
    audio,
    currentTrackId,
    maybeEmitSkip,
  ]);

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function usePlayer(): PlayerCtx {
  const v = useContext(Ctx);
  if (!v) throw new Error("usePlayer must be used inside <PlayerProvider>");
  return v;
}
