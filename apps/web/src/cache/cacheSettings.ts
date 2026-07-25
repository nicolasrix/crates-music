// Audio-cache budgets, persisted in localStorage. Mirrors the
// player/autoplaySettings.ts pattern (STORAGE_KEY + clamped load/save) so
// a hand-edited blob or out-of-range slider can never push the cache into
// a degenerate state.
//
// Two budgets, exactly like crates/music-cache:
//   • regular — LRU-evicted; auto-on-play recents live here.
//   • pinned  — never LRU-evicted; explicit "save for offline" lives here.

const MB = 1024 * 1024;

/** Transcode-to-fit: what to ask Navidrome for when fetching audio for the
 *  cache. The gateway's /rest proxy forwards `format`/`maxBitRate` verbatim
 *  to Navidrome's stream endpoint (the same mechanism ingest uses), so no
 *  gateway work is involved. "original" = passthrough, no transcode. */
export type DownloadQuality = "original" | "opus128" | "mp3128";

export const DOWNLOAD_QUALITIES: readonly DownloadQuality[] = [
  "original",
  "opus128",
  "mp3128",
];

/** Stream-request params for a quality, or null for passthrough. The
 *  `bitrate` mirrors maxBitRate and becomes the cache key's bitrate field. */
export function qualityParams(
  q: DownloadQuality,
): { format: string; maxBitRate: number } | null {
  switch (q) {
    case "opus128":
      return { format: "opus", maxBitRate: 128 };
    case "mp3128":
      return { format: "mp3", maxBitRate: 128 };
    case "original":
      return null;
  }
}

/** Just the byte budgets — what the cache engine itself depends on
 *  (audioCache.ts); the quality knob is a fetch-time concern. */
export interface CacheBudgets {
  /** Byte cap for LRU-evictable (auto-cached) audio. */
  regularBudgetBytes: number;
  /** Separate byte cap for pinned ("downloaded") audio, never LRU-evicted. */
  pinnedBudgetBytes: number;
}

export interface CacheSettings extends CacheBudgets {
  /** Transcode target for newly cached audio (downloads + auto-cache). */
  downloadQuality: DownloadQuality;
}

export const DEFAULT_CACHE_SETTINGS: CacheSettings = {
  // ~500 MB total default for the web client (per CLAUDE.md), split
  // regular-heavy: recents churn, downloads are deliberate.
  regularBudgetBytes: 400 * MB,
  pinnedBudgetBytes: 100 * MB,
  downloadQuality: "original",
};

/** Bounds for clamping + the Settings sliders. Step is 50 MB. */
export const CACHE_BOUNDS = {
  regularBudgetBytes: { min: 0, max: 8 * 1024 * MB, step: 50 * MB },
  pinnedBudgetBytes: { min: 0, max: 8 * 1024 * MB, step: 50 * MB },
} as const;

const STORAGE_KEY = "crates-music.cache.settings";

function clampField(key: keyof typeof CACHE_BOUNDS, value: unknown): number {
  const b = CACHE_BOUNDS[key];
  const n = typeof value === "number" && Number.isFinite(value) ? value : NaN;
  if (Number.isNaN(n)) return DEFAULT_CACHE_SETTINGS[key];
  return Math.min(b.max, Math.max(b.min, n));
}

function normalizeQuality(value: unknown): DownloadQuality {
  return DOWNLOAD_QUALITIES.includes(value as DownloadQuality)
    ? (value as DownloadQuality)
    : DEFAULT_CACHE_SETTINGS.downloadQuality;
}

export function normalizeCacheSettings(raw: unknown): CacheSettings {
  const obj = (raw ?? {}) as Record<string, unknown>;
  return {
    regularBudgetBytes: clampField("regularBudgetBytes", obj.regularBudgetBytes),
    pinnedBudgetBytes: clampField("pinnedBudgetBytes", obj.pinnedBudgetBytes),
    downloadQuality: normalizeQuality(obj.downloadQuality),
  };
}

export function loadCacheSettings(): CacheSettings {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULT_CACHE_SETTINGS };
    return normalizeCacheSettings(JSON.parse(raw));
  } catch {
    return { ...DEFAULT_CACHE_SETTINGS };
  }
}

export function saveCacheSettings(settings: CacheSettings): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(normalizeCacheSettings(settings)));
  } catch {
    /* localStorage may be unavailable (private mode); ignore */
  }
}
