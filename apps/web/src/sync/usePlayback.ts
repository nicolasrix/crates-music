// Click-handler-friendly playback API. Wraps SyncContext + PlayerContext
// so callers don't need to know about the two-step "prime audio, then
// submit ops" dance.
//
// Why both contexts: SyncContext owns the authoritative queue/cursor
// state and round-trips ops through the gateway. PlayerContext owns
// the local <audio> element. The browser's autoplay policy treats
// audio.play() leniently when called *during* a user gesture and
// strictly otherwise; once the WS roundtrip completes, the gesture
// is effectively gone and a deferred play() can be silently rejected.
// So we play locally first, then submit the sync ops — the gateway
// catches up and broadcasts confirmation; the audio doesn't wait.

import { useCallback } from "react";
import type { Track } from "../api/types";
import { usePlayer } from "../player/PlayerContext";
import {
  playList as playListImpl,
  playSingle as playSingleImpl,
} from "./playbackHelpers";
import { useSync } from "./SyncContext";

interface Playback {
  playSingle: (track: Track) => void;
  playList: (tracks: readonly Track[], startIndex: number) => void;
}

export function usePlayback(): Playback {
  const sync = useSync();
  const { primePlayback } = usePlayer();

  const playSingle = useCallback(
    (track: Track) => {
      primePlayback(track);
      playSingleImpl(sync, track);
    },
    [sync, primePlayback]
  );

  const playList = useCallback(
    (tracks: readonly Track[], startIndex: number) => {
      const t = tracks[startIndex];
      if (t) primePlayback(t);
      playListImpl(sync, tracks, startIndex);
    },
    [sync, primePlayback]
  );

  return { playSingle, playList };
}
