// The play mode: how a list of songs is turned into a queue.
//
//   in_order      — the context as it stands (album order, playlist order…)
//   shuffle       — the context, reordered
//   smart_shuffle — the context, reordered, with recommendations mixed
//                   through it (Spotify calls this Smart Shuffle)
//
// Distinct from autoplay, which is about what happens when the queue runs
// *out*: autoplay appends a station at the end, smart shuffle salts the
// list you're already playing. Both can be on; they don't overlap.
//
// Persisted in localStorage so the mode survives a reload — an installed
// PWA relaunches often enough that a forgotten shuffle setting reads as
// a bug.

export type PlayMode = "in_order" | "shuffle" | "smart_shuffle";

export const PLAY_MODES: readonly PlayMode[] = [
  "in_order",
  "shuffle",
  "smart_shuffle",
];

const STORAGE_KEY = "crates-music.playMode";

/** The order the player-bar button steps through. */
export function nextMode(mode: PlayMode): PlayMode {
  const i = PLAY_MODES.indexOf(mode);
  return PLAY_MODES[(i + 1) % PLAY_MODES.length]!;
}

/** True when the mode reorders the context (i.e. either shuffle flavour). */
export function isShuffled(mode: PlayMode): boolean {
  return mode !== "in_order";
}

export function loadPlayMode(): PlayMode {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    return PLAY_MODES.includes(v as PlayMode) ? (v as PlayMode) : "in_order";
  } catch {
    return "in_order";
  }
}

export function savePlayMode(mode: PlayMode): void {
  try {
    localStorage.setItem(STORAGE_KEY, mode);
  } catch {
    /* localStorage may be unavailable (private mode); ignore */
  }
}

// The started-from context, persisted alongside the mode so "shuffle off"
// still restores the original order after a reload. Ids only — metadata
// is re-hydrated by SyncContext from the queue itself.
const CONTEXT_KEY = "crates-music.playContext";

export interface StoredContext {
  sessionId: string;
  trackIds: string[];
}

export function loadStoredContext(): StoredContext | null {
  try {
    const raw = localStorage.getItem(CONTEXT_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<StoredContext>;
    if (typeof parsed.sessionId !== "string" || !Array.isArray(parsed.trackIds)) {
      return null;
    }
    return { sessionId: parsed.sessionId, trackIds: parsed.trackIds.filter(isString) };
  } catch {
    return null;
  }
}

export function saveStoredContext(ctx: StoredContext | null): void {
  try {
    if (ctx === null) localStorage.removeItem(CONTEXT_KEY);
    else localStorage.setItem(CONTEXT_KEY, JSON.stringify(ctx));
  } catch {
    /* ignore */
  }
}

function isString(v: unknown): v is string {
  return typeof v === "string";
}
