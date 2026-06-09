// Shared transcode-quality option list. Used by both the Playback panel
// (live streaming target) and the Storage panel (offline-download target)
// — same DownloadQuality values, same labels, so it lives in one place.

import { DOWNLOAD_QUALITIES, DownloadQuality } from "../cache/cacheSettings";

const QUALITY_LABELS: Record<DownloadQuality, string> = {
  original: "original (no transcode)",
  opus128: "opus · 128 kbps (smallest)",
  mp3128: "mp3 · 128 kbps (most compatible)",
};

export const QUALITY_OPTIONS = DOWNLOAD_QUALITIES.map((q) => ({
  value: q,
  label: QUALITY_LABELS[q],
}));
