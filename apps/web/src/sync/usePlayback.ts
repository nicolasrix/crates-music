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

import { useQueryClient } from "@tanstack/react-query";
import { useCallback } from "react";
import { getAlbum } from "../api/client";
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
  /** Fetch an album's tracks and play them from the top. Backs the
   *  play overlays on album tiles / hero cards / table rows, where
   *  the surface only holds an Album (no tracklist). */
  playAlbum: (albumId: string) => Promise<void>;
}

export function usePlayback(): Playback {
  const sync = useSync();
  const { primePlayback } = usePlayer();
  const queryClient = useQueryClient();

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

  const playAlbum = useCallback(
    async (albumId: string) => {
      // Shares the album-detail page's query key, so a warm cache
      // resolves in a microtask and the click's transient user
      // activation survives for the audio.play() inside primePlayback.
      // A cold LAN fetch is well inside the activation window too.
      const { tracks } = await queryClient.fetchQuery({
        queryKey: ["album", albumId],
        queryFn: () => getAlbum(albumId),
        staleTime: 5 * 60_000,
      });
      if (tracks.length === 0) return;
      primePlayback(tracks[0]!);
      playListImpl(sync, tracks, 0);
    },
    [sync, primePlayback, queryClient]
  );

  return { playSingle, playList, playAlbum };
}
