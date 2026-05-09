// Audio shell: holds the single <audio> element and reflects the
// authoritative sync state into it. State (queue, cursor, play/pause)
// lives in SyncContext — this layer is just glue between that data and
// the browser's audio API.
//
// On track end we submit a SetNowPlaying op for the next index (or
// SetPlaying(false) at the end of the queue). The eventual broadcast
// flips local state and this effect picks up the change.

import { createContext, ReactNode, useContext, useEffect, useMemo, useRef, useState } from "react";
import { coverArtUrl, streamUrl } from "../api/client";
import { markEvent } from "../rum";
import { useSync } from "../sync/SyncContext";
import type { Track } from "../api/types";

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
  const { queue, now_playing_index, is_playing } = state.playback;
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

  const currentItem = now_playing_index !== null ? queue.items[now_playing_index] : undefined;
  const currentTrackId = currentItem?.track_id;
  const nowPlaying: Track | null =
    currentTrackId !== undefined ? trackMeta.get(currentTrackId) ?? null : null;

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
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    if (!currentTrackId) {
      audio.pause();
      return;
    }
    audio.src = streamUrl(currentTrackId);
    startTsRef.current = performance.now();
    if (is_playing) void audio.play().catch(() => {});
  }, [currentTrackId]); // eslint-disable-line react-hooks/exhaustive-deps

  // One-shot `playing` listener per src change emits the latency mark.
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio || !currentTrackId) return;
    const onPlaying = () => {
      const start = startTsRef.current;
      if (start === null) return;
      const elapsed = performance.now() - start;
      startTsRef.current = null; // one-shot — don't double-emit on resume
      markEvent("playback.start", {
        value_ms: elapsed,
        fields: { track_id: currentTrackId },
      });
    };
    audio.addEventListener("playing", onPlaying);
    return () => audio.removeEventListener("playing", onPlaying);
  }, [currentTrackId]);

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
      const artUrl = nowPlaying.coverArt ? coverArtUrl(nowPlaying.coverArt, 512) : null;
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
    ms.setActionHandler("play", () => submit({ type: "set_playing", is_playing: true }));
    ms.setActionHandler("pause", () => submit({ type: "set_playing", is_playing: false }));
    ms.setActionHandler("nexttrack", () => {
      if (i !== null && i + 1 < total) submit({ type: "set_now_playing", index: i + 1 });
    });
    ms.setActionHandler("previoustrack", () => {
      if (i !== null && i > 0) submit({ type: "set_now_playing", index: i - 1 });
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
  }, [now_playing_index, queue.items.length, submit]);

  // Auto-advance on end. Submitting an op rather than mutating local
  // state keeps the gateway authoritative.
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    const handler = () => {
      const i = now_playing_index;
      if (i === null) return;
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
          submit({ type: "set_now_playing", index: i + 1 });
        }
      },
      prev: () => {
        if (i !== null && i > 0) {
          submit({ type: "set_now_playing", index: i - 1 });
        }
      },
      primePlayback: (track) => {
        const a = audioRef.current;
        if (!a) return;
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
  }, [nowPlaying, is_playing, queue.items.length, now_playing_index, submit, audio]);

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function usePlayer(): PlayerCtx {
  const v = useContext(Ctx);
  if (!v) throw new Error("usePlayer must be used inside <PlayerProvider>");
  return v;
}
