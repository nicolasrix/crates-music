// Single audio element + queue state. Plain <audio> — MSE / gapless
// were deferred at P3 design time; revisit if/when gap-perception
// matters in the browser.

import { createContext, ReactNode, useContext, useEffect, useRef, useState } from "react";
import { Track } from "../api/types";
import { streamUrl } from "../api/client";

interface PlayerState {
  queue: Track[];
  index: number;
  playing: boolean;
  setQueue: (tracks: Track[], startIndex?: number) => void;
  togglePlay: () => void;
  next: () => void;
  prev: () => void;
}

const Ctx = createContext<PlayerState | null>(null);

export function PlayerProvider({ children }: { children: ReactNode }) {
  const [queue, setQueueState] = useState<Track[]>([]);
  const [index, setIndex] = useState(0);
  const [playing, setPlaying] = useState(false);
  const audioRef = useRef<HTMLAudioElement | null>(null);

  // Lazily construct the audio element so SSR / first paint don't
  // touch DOM APIs.
  useEffect(() => {
    if (!audioRef.current) {
      audioRef.current = new Audio();
      audioRef.current.preload = "auto";
      audioRef.current.addEventListener("ended", () => {
        setIndex((i) => (i + 1 < queue.length ? i + 1 : i));
      });
    }
  }, [queue.length]);

  // Whenever the active track changes, swap the src and (try to)
  // resume playback. Browsers may block autoplay on first load — the
  // user's explicit play click satisfies that requirement.
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    const track = queue[index];
    if (!track) {
      audio.pause();
      setPlaying(false);
      return;
    }
    audio.src = streamUrl(track.id);
    if (playing) {
      void audio.play().catch(() => setPlaying(false));
    }
  }, [queue, index]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    if (playing) void audio.play().catch(() => setPlaying(false));
    else audio.pause();
  }, [playing]);

  const value: PlayerState = {
    queue,
    index,
    playing,
    setQueue: (tracks, startIndex = 0) => {
      setQueueState(tracks);
      setIndex(startIndex);
      setPlaying(true);
    },
    togglePlay: () => setPlaying((p) => !p),
    next: () => setIndex((i) => Math.min(i + 1, queue.length - 1)),
    prev: () => setIndex((i) => Math.max(i - 1, 0)),
  };
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function usePlayer(): PlayerState {
  const v = useContext(Ctx);
  if (!v) throw new Error("usePlayer must be used inside <PlayerProvider>");
  return v;
}
