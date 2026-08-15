// Per-track timing nudge for LRC files that run early or late.
//
// Sign convention, stated once because it is the only confusing part:
// `offsetMs` is *added to the playback position before looking up a line*.
// A positive offset therefore looks further into the track, so lines light
// up sooner — "lyrics earlier". Negative holds each line longer — "lyrics
// later". Seeking runs the same arithmetic backwards (`start_ms - offset`).
//
// Stored client-side, in one localStorage key rather than one key per
// track: a per-track key would litter the namespace with unbounded entries
// that nothing ever collects, and `clearUserData`'s prefix sweep would then
// be doing an unbounded amount of work on sign-out. The `crates-music.`
// prefix means that sweep still picks this up.
//
// Kept local rather than sent to the gateway on purpose. A drifting LRC is
// a property of the *file* and would be worth sharing — but the fix belongs
// upstream at LRCLIB, and a per-user local nudge is the version that needs
// no schema, no endpoint and no guest-permission question. Promote it if it
// proves to be more than an occasional rescue.

const STORAGE_KEY = "crates-music.lyrics.offsets";

/** One tap. A quarter second is about the smallest drift a listener will
 *  reliably notice against a sung line. */
export const OFFSET_STEP_MS = 250;

/** Nudges are for drift, not for repair: past a few seconds the document is
 *  the wrong song and "look again" is the real fix. */
export const OFFSET_LIMIT_MS = 5_000;

/** How many tracks keep a remembered nudge. Oldest-written fall off first. */
export const MAX_REMEMBERED = 200;

export type OffsetMap = Readonly<Record<string, number>>;

function clamp(ms: number): number {
  if (!Number.isFinite(ms)) return 0;
  // Snap to the step so a hand-edited blob can't produce a value the ± UI
  // is unable to walk back to zero.
  const snapped = Math.round(ms / OFFSET_STEP_MS) * OFFSET_STEP_MS;
  return Math.min(OFFSET_LIMIT_MS, Math.max(-OFFSET_LIMIT_MS, snapped));
}

export function normalizeOffsets(raw: unknown): OffsetMap {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return {};
  const out: Record<string, number> = {};
  for (const [trackId, value] of Object.entries(raw as Record<string, unknown>)) {
    if (typeof value !== "number") continue;
    const ms = clamp(value);
    // Zero is the default; storing it would waste a slot in the ring.
    if (ms !== 0) out[trackId] = ms;
  }
  return out;
}

/**
 * A new map with `trackId` set to `ms` (clamped), pruned to MAX_REMEMBERED.
 *
 * The key is deleted and re-added rather than assigned in place, because
 * JS object key order is insertion order for string keys — re-adding moves
 * the track to the young end, so the prune below drops genuinely stale
 * entries instead of whichever track happened to be nudged first.
 */
export function withOffset(map: OffsetMap, trackId: string, ms: number): OffsetMap {
  const clamped = clamp(ms);
  const rest = Object.entries(map).filter(([id]) => id !== trackId);
  if (clamped === 0) return Object.fromEntries(rest);
  const kept = rest.slice(Math.max(0, rest.length - (MAX_REMEMBERED - 1)));
  return Object.fromEntries([...kept, [trackId, clamped] as const]);
}

export function loadOffsets(): OffsetMap {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    return normalizeOffsets(JSON.parse(raw));
  } catch {
    return {};
  }
}

export function saveOffsets(map: OffsetMap): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(map));
  } catch {
    /* localStorage may be unavailable (private mode); the nudge still
       applies for this session, it just won't be remembered */
  }
}

/** The remembered nudge for one track, or 0. */
export function readOffset(trackId: string): number {
  return loadOffsets()[trackId] ?? 0;
}

/** Persist a nudge and return the value actually stored (post-clamp), so
 *  the caller's state and localStorage cannot disagree at the limits. */
export function writeOffset(trackId: string, ms: number): number {
  const next = withOffset(loadOffsets(), trackId, ms);
  saveOffsets(next);
  return next[trackId] ?? 0;
}

/** Human-readable nudge, e.g. `+0.5s`. Empty string at zero — the control
 *  shows nothing rather than a meaningless `0.0s`. */
export function formatOffset(ms: number): string {
  if (ms === 0) return "";
  const sign = ms > 0 ? "+" : "−";
  return `${sign}${(Math.abs(ms) / 1000).toFixed(2).replace(/0$/, "")}s`;
}
