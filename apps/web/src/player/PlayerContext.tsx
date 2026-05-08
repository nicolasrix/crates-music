// Audio shell: holds the single <audio> element and reflects the
// authoritative sync state into it. State (queue, cursor, play/pause)
// lives in SyncContext — this layer is just glue between that data and
// the browser's audio API.
//
// On track end we submit a SetNowPlaying op for the next index (or
// SetPlaying(false) at the end of the queue). The eventual broadcast
// flips local state and this effect picks up the change.

import { createContext, ReactNode, useContext, useEffect, useMemo, useRef } from "react";
import { streamUrl } from "../api/client";
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
}

const Ctx = createContext<PlayerCtx | null>(null);

export function PlayerProvider({ children }: { children: ReactNode }) {
  const { state, trackMeta, submit } = useSync();
  const { queue, now_playing_index, is_playing } = state.playback;
  const audioRef = useRef<HTMLAudioElement | null>(null);

  const currentItem = now_playing_index !== null ? queue.items[now_playing_index] : undefined;
  const currentTrackId = currentItem?.track_id;
  const nowPlaying: Track | null =
    currentTrackId !== undefined ? trackMeta.get(currentTrackId) ?? null : null;

  // Lazy-construct the <audio> on the client. SSR-safe (we don't SSR
  // anyway, but the principle is cheap to keep).
  useEffect(() => {
    if (!audioRef.current) {
      audioRef.current = new Audio();
      audioRef.current.preload = "auto";
    }
  }, []);

  // Swap src when the now-playing track changes. The track id (not the
  // index) is the right dependency: reordering the queue under the
  // cursor is a no-op for playback.
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    if (!currentTrackId) {
      audio.pause();
      return;
    }
    audio.src = streamUrl(currentTrackId);
    if (is_playing) void audio.play().catch(() => {});
  }, [currentTrackId]); // eslint-disable-line react-hooks/exhaustive-deps

  // Reflect the play/pause flag.
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    if (is_playing) void audio.play().catch(() => {});
    else audio.pause();
  }, [is_playing]);

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
    };
  }, [nowPlaying, is_playing, queue.items.length, now_playing_index, submit]);

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function usePlayer(): PlayerCtx {
  const v = useContext(Ctx);
  if (!v) throw new Error("usePlayer must be used inside <PlayerProvider>");
  return v;
}
