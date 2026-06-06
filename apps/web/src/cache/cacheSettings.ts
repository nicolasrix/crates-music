// Audio-cache budgets, persisted in localStorage. Mirrors the
// player/autoplaySettings.ts pattern (STORAGE_KEY + clamped load/save) so
// a hand-edited blob or out-of-range slider can never push the cache into
// a degenerate state.
//
// Two budgets, exactly like crates/music-cache:
//   • regular — LRU-evicted; auto-on-play recents live here.
//   • pinned  — never LRU-evicted; explicit "save for offline" lives here.

const MB = 1024 * 1024;

export interface CacheSettings {
  /** Byte cap for LRU-evictable (auto-cached) audio. */
  regularBudgetBytes: number;
  /** Separate byte cap for pinned ("downloaded") audio, never LRU-evicted. */
  pinnedBudgetBytes: number;
}

export const DEFAULT_CACHE_SETTINGS: CacheSettings = {
  // ~500 MB total default for the web client (per CLAUDE.md), split
  // regular-heavy: recents churn, downloads are deliberate.
  regularBudgetBytes: 400 * MB,
  pinnedBudgetBytes: 100 * MB,
};

/** Bounds for clamping + the Settings sliders. Step is 50 MB. */
export const CACHE_BOUNDS = {
  regularBudgetBytes: { min: 0, max: 8 * 1024 * MB, step: 50 * MB },
  pinnedBudgetBytes: { min: 0, max: 8 * 1024 * MB, step: 50 * MB },
} as const;

const STORAGE_KEY = "crates-music.cache.settings";

function clampField(key: keyof CacheSettings, value: unknown): number {
  const b = CACHE_BOUNDS[key];
  const n = typeof value === "number" && Number.isFinite(value) ? value : NaN;
  if (Number.isNaN(n)) return DEFAULT_CACHE_SETTINGS[key];
  return Math.min(b.max, Math.max(b.min, n));
}

export function normalizeCacheSettings(raw: unknown): CacheSettings {
  const obj = (raw ?? {}) as Record<string, unknown>;
  return {
    regularBudgetBytes: clampField("regularBudgetBytes", obj.regularBudgetBytes),
    pinnedBudgetBytes: clampField("pinnedBudgetBytes", obj.pinnedBudgetBytes),
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
