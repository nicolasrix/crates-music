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
  startSession: (tracks: readonly Track[], anchorIndex: number) => void;
}

/** Replace the queue with the given tracks and start playing at
 *  `startIndex`. Used when the user explicitly invokes "play whole
 *  album / playlist" from the hero button, or anchors a tracklist
 *  at a specific track. */
export function playList(
  sync: SyncSubmitter,
  tracks: readonly Track[],
  startIndex: number,
): void {
  if (tracks.length === 0) return;
  sync.startSession(tracks, startIndex);
}

/** Replace the queue with a single track and play it. Default for
 *  clicking a track in a list — earlier behaviour pushed the
 *  surrounding context (whole album/playlist) which surprised users
 *  who just wanted to hear one song. */
export function playSingle(sync: SyncSubmitter, track: Track): void {
  sync.startSession([track], 0);
}
