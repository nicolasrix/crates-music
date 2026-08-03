// Pin a list of tracks for offline, one at a time, tolerating per-track
// failures but stopping dead when the budget runs out.
//
// Shared by DownloadAllButton (album / playlist hero action, renders
// inline N/M progress) and the album / artist row menus (fire-and-toast).
// Sequential on purpose: parallel pins race each other's budget checks,
// and a phone on a weak link does worse with six concurrent fetches than
// with six sequential ones.

import type { PinOutcome } from "./audioCache";

/** The slice of AudioCacheContext this needs. Narrow on purpose — it
 *  makes the loop testable against a plain object. */
export interface TrackPinner {
  download: (trackId: string) => Promise<PinOutcome>;
}

export interface BulkDownloadResult {
  /** Tracks that ended up pinned (including ones already pinned). */
  saved: number;
  /** Tracks whose fetch threw — offline, or a catalog gap. */
  failed: number;
  /** Bytes the budget fell short by, when that's what stopped the run.
   *  `null` means the run reached the end of the list. */
  shortBy: number | null;
}

export async function downloadTracks(
  cache: TrackPinner,
  trackIds: readonly string[],
  onProgress?: (done: number, total: number) => void,
): Promise<BulkDownloadResult> {
  let saved = 0;
  let failed = 0;
  for (let i = 0; i < trackIds.length; i++) {
    try {
      const outcome = await cache.download(trackIds[i]!);
      // Budget exhaustion is terminal: every subsequent pin would fail
      // the same check, so continuing just burns bandwidth on fetches
      // whose blobs we then refuse to keep.
      if (outcome.kind === "would-exceed-budget") {
        return { saved, failed, shortBy: outcome.overBy };
      }
      saved++;
    } catch {
      // Count rather than swallow — reporting "N/N saved" while tracks
      // are missing lies about what's actually playable offline.
      failed++;
    }
    onProgress?.(i + 1, trackIds.length);
  }
  return { saved, failed, shortBy: null };
}
