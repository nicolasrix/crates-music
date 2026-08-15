// Shared playback helpers built on top of SyncContext. Centralised here
// so every page that renders a tracklist applies the same semantics —
// previously each page had its own `playFrom` and they drifted.
//
// "Play this" is now a single atomic `start_session` op: it replaces
// the queue, sets the cursor, and starts a fresh recommend-session,
// all server-applied in one shot. Pre-session this was four ops
// (clear + push×N + set_now_playing + set_playing) and observers
// could briefly see a cleared queue with no session intent.

import type { Track } from "../api/types";

interface SyncSubmitter {
  startSession: (tracks: readonly Track[], anchorIndex: number) => string;
}

/** Replace the queue with the given tracks and start playing at
 *  `startIndex`. The list arrives here already in play order — the play
 *  mode does its shuffling upstream, in PlayModeContext — so this stays
 *  the one dumb "make this exact list the queue" primitive.
 *
 *  Returns the new session id (empty string for a no-op), which callers
 *  use to file the context the queue was built from. */
export function playList(
  sync: SyncSubmitter,
  tracks: readonly Track[],
  startIndex: number,
): string {
  if (tracks.length === 0) return "";
  return sync.startSession(tracks, startIndex);
}

/** Replace the queue with a single track and play it. For surfaces with
 *  no surrounding list to queue (the latent-space scatter plots); a
 *  track clicked inside a *list* takes that list with it — see
 *  `usePlayback.playList`. */
export function playSingle(sync: SyncSubmitter, track: Track): void {
  sync.startSession([track], 0);
}
