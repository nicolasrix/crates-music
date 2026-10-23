// Liveness checks for the sync WebSocket, kept pure so they can be
// exercised without a socket (same shape as reconnect.ts).
//
// A browser WebSocket whose network was silently dropped — a phone that
// slept, a Wi-Fi radio in power save — keeps reporting `OPEN`, and
// `send()` on it succeeds into the void. Nothing ever fires `onclose`.
// The only way to find out is to ask: send an app-level `ping` and treat
// a missing `pong` as a dead socket.

/** How often an open socket is pinged. */
export const HEARTBEAT_INTERVAL_MS = 15_000;
/** How long a ping may go unanswered before the socket is written off.
 *  A healthy round trip is milliseconds, so this is pure slack for a
 *  congested cellular link. */
export const PONG_TIMEOUT_MS = 10_000;
/** Tighter deadline for the probe sent when the app returns to the
 *  foreground or the network comes back — the moment a user is looking. */
export const PROBE_TIMEOUT_MS = 5_000;

/**
 * Whether the ping outstanding since `awaitingSinceMs` has gone
 * unanswered past `timeoutMs`.
 *
 * Keyed on the *outstanding ping*, not on "time since the last frame":
 * an idle room sends nothing, and a background tab's timers can be
 * throttled for minutes, so silence alone proves nothing. A sent ping
 * that got no reply does.
 */
export function pingOverdue(
  awaitingSinceMs: number | null,
  nowMs: number,
  timeoutMs: number = PONG_TIMEOUT_MS,
): boolean {
  return awaitingSinceMs !== null && nowMs - awaitingSinceMs > timeoutMs;
}
