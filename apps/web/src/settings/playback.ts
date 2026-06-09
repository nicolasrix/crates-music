// Streaming quality, persisted in localStorage. The *streaming* counterpart
// to the offline-cache download quality (cache/cacheSettings.ts).
//
// Two independent knobs on purpose:
//   • download quality  — transcode target for bytes we KEEP (offline cache).
//   • stream quality     — transcode target for live <audio> playback of
//     tracks that aren't cached. Lets a metered mobile connection stream
//     opus@128 without permanently degrading what gets saved for offline.
//
// Both reuse the same DownloadQuality codec/bitrate mapping (qualityParams),
// and both ride the gateway's verbatim /rest proxy — Navidrome transcodes
// server-side from `format`/`maxBitRate`. "original" = passthrough.

import { DownloadQuality, DOWNLOAD_QUALITIES, qualityParams } from "../cache/cacheSettings";

export type StreamQuality = DownloadQuality;

export const STREAM_QUALITIES = DOWNLOAD_QUALITIES;

export const DEFAULT_STREAM_QUALITY: StreamQuality = "original";

const STORAGE_KEY = "crates-music.playback.streamQuality";

export function loadStreamQuality(): StreamQuality {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    return DOWNLOAD_QUALITIES.includes(v as StreamQuality)
      ? (v as StreamQuality)
      : DEFAULT_STREAM_QUALITY;
  } catch {
    return DEFAULT_STREAM_QUALITY;
  }
}

export function saveStreamQuality(q: StreamQuality): void {
  try {
    localStorage.setItem(STORAGE_KEY, q);
  } catch {
    /* localStorage may be unavailable (private mode); ignore */
  }
}

/** `format`/`maxBitRate` query params for the active stream quality, or
 *  null for passthrough. Read at <audio> src-build time (streamUrl). */
export function streamQualityParams(): { format: string; maxBitRate: number } | null {
  return qualityParams(loadStreamQuality());
}
