// Turns an "add to playlist" outcome into the toast the user sees.
//
// Lives apart from the three surfaces that add tracks (the row-menu
// picker, the sidebar drop target, the playlist page's suggestions) so
// they can't drift into wording each other's copy differently — the same
// duplicate says the same thing everywhere.
//
// The gateway skips ids a playlist already holds and reports `skipped`;
// this is where that count becomes prose.

import type { PlaylistAddResult } from "../api/playlists";
import type { ToastVariant } from "../toast/ToastContext";

export interface PlaylistAddMessage {
  message: string;
  variant: ToastVariant;
}

function tracks(n: number): string {
  return n === 1 ? "1 track" : `${n} tracks`;
}

export function playlistAddMessage(
  playlistName: string,
  { added, skipped }: PlaylistAddResult,
): PlaylistAddMessage {
  const target = `“${playlistName}”`;

  if (added === 0) {
    // Nothing landed. Duplicates are the interesting case and the reason
    // this helper exists — say which one it was rather than a silent
    // success the user would read as "it worked".
    if (skipped === 0) return { message: `nothing to add to ${target}`, variant: "info" };
    const message =
      skipped === 1
        ? `already in ${target}`
        : `all ${skipped} tracks are already in ${target}`;
    return { message, variant: "info" };
  }

  if (skipped > 0) {
    const were = skipped === 1 ? "was" : "were";
    return {
      message: `added ${tracks(added)} to ${target} — ${skipped} ${were} already there`,
      variant: "success",
    };
  }

  // The common path: keep the single-track phrasing short, since it's the
  // one a user triggers dozens of times a session.
  const message = added === 1 ? `added to ${target}` : `added ${added} tracks to ${target}`;
  return { message, variant: "success" };
}
