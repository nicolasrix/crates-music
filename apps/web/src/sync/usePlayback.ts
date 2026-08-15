// Click-handler-friendly playback API. Wraps SyncContext + PlayerContext
// + PlayModeContext so callers don't need to know about the two-step
// "prime audio, then submit ops" dance.
//
// Why all three: SyncContext owns the authoritative queue/cursor state
// and round-trips ops through the gateway. PlayerContext owns the local
// <audio> element. PlayModeContext decides what order a list is queued
// in (and mixes recommendations into it). The browser's autoplay policy
// treats audio.play() leniently when called *during* a user gesture and
// strictly otherwise; once the WS roundtrip completes, the gesture is
// effectively gone and a deferred play() can be silently rejected. So we
// play locally first, then submit the sync ops — the gateway catches up
// and broadcasts confirmation; the audio doesn't wait.
//
// `playList` is what a tracklist row click ends up in: playing one song
// out of a list queues the whole list around it (album, playlist, liked
// songs, artist top songs, search results…), the way every other player
// behaves. `playSingle` survives for the surfaces that genuinely have no
// list — the latent-space scatter plots.

import { useQueryClient } from "@tanstack/react-query";
import { useCallback } from "react";
import { getAlbum } from "../api/client";
import type { Track } from "../api/types";
import type { PlayMode } from "../player/playMode";
import { usePlayMode } from "../player/PlayModeContext";
import { usePlayer } from "../player/PlayerContext";
import { playSingle as playSingleImpl } from "./playbackHelpers";
import { useSync } from "./SyncContext";

interface Playback {
  /** Queue exactly one track, with no surrounding context. */
  playSingle: (track: Track) => void;
  /** Queue `tracks` as the current context and start at `startIndex`.
   *  The clicked track plays first; the play mode decides the order of
   *  everything after it. `modeOverride` is for the surfaces that ask
   *  for a mode explicitly (a "shuffle playlist" button) — it also
   *  becomes the active mode. */
  playList: (
    tracks: readonly Track[],
    startIndex: number,
    modeOverride?: PlayMode,
  ) => void;
  /** Fetch an album's tracks and play them from the top. Backs the
   *  play overlays on album tiles / hero cards / table rows, where
   *  the surface only holds an Album (no tracklist). */
  playAlbum: (albumId: string) => Promise<void>;
}

export function usePlayback(): Playback {
  const sync = useSync();
  const { primePlayback } = usePlayer();
  const { startContext } = usePlayMode();
  const queryClient = useQueryClient();

  const playSingle = useCallback(
    (track: Track) => {
      primePlayback(track);
      playSingleImpl(sync, track);
    },
    [sync, primePlayback]
  );

  const playList = useCallback(
    (tracks: readonly Track[], startIndex: number, modeOverride?: PlayMode) => {
      // Prime the *clicked* track, not `ordered[0]` — they're the same
      // by construction (startOrder puts the pick first), and reading it
      // from the caller's array keeps the gesture path free of any
      // dependency on how the mode reorders things.
      const t = tracks[startIndex];
      if (t) primePlayback(t);
      startContext(tracks, startIndex, modeOverride);
    },
    [startContext, primePlayback]
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
      startContext(tracks, 0);
    },
    [startContext, primePlayback, queryClient]
  );

  return { playSingle, playList, playAlbum };
}
