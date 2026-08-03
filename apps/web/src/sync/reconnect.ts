// Backoff schedule for the sync WebSocket, kept pure so it can be
// exercised without a socket (same shape as scrobble.ts / skip.ts).

/** First retry lands fast — most closes are a momentary blip. */
export const RECONNECT_BASE_MS = 500;
/** Ceiling. A phone that has been in a tunnel for an hour should still
 *  be probing at least twice a minute, so this stays well under the
 *  access-token lifetime. */
export const RECONNECT_MAX_MS = 30_000;

/** Attempt index past which doubling is pointless (2^16 · base already
 *  exceeds the cap by orders of magnitude). Clamped so a long-lived tab
 *  can't drive `2 ** attempt` to Infinity. */
const MAX_DOUBLINGS = 16;

/**
 * Delay before reconnect attempt `attempt` (0-based).
 *
 * Exponential with a cap, then "equal jitter": half the window is fixed,
 * half is random. The jitter matters because every device on an account
 * drops at the same instant when the gateway restarts — without it they
 * all come back in lockstep and hammer the same second.
 *
 * `random` is injectable purely so tests can pin it.
 */
export function reconnectDelayMs(attempt: number, random: () => number = Math.random): number {
  const clamped = Math.min(Math.max(attempt, 0), MAX_DOUBLINGS);
  const window = Math.min(RECONNECT_BASE_MS * 2 ** clamped, RECONNECT_MAX_MS);
  return Math.round(window / 2 + random() * (window / 2));
}
