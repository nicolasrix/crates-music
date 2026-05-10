// Shared playback helpers built on top of SyncContext. Centralised here
// so every page that renders a tracklist applies the same semantics —
// previously each page had its own `playFrom` and they drifted.

import type { Track } from "../api/types";

interface SyncSubmitter {
  submit: (op: import("./types").SyncOp) => void;
  pushTrack: (track: Track) => string;
}

/** Replace the queue with the given tracks and start playing at
 *  `startIndex`. Used when the user explicitly invokes "play whole
 *  album / playlist" from the hero button. */
export function playList(
  sync: SyncSubmitter,
  tracks: readonly Track[],
  startIndex: number
): void {
  if (tracks.length === 0) return;
  sync.submit({ type: "clear" });
  for (const t of tracks) sync.pushTrack(t);
  sync.submit({ type: "set_now_playing", index: startIndex });
  sync.submit({ type: "set_playing", is_playing: true });
}

/** Replace the queue with a single track and play it. This is the
 *  default for clicking a track in a list — earlier behaviour pushed
 *  the surrounding context (whole album/playlist) which surprised
 *  users when they just wanted to hear one song. */
export function playSingle(sync: SyncSubmitter, track: Track): void {
  sync.submit({ type: "clear" });
  sync.pushTrack(track);
  sync.submit({ type: "set_now_playing", index: 0 });
  sync.submit({ type: "set_playing", is_playing: true });
}
