// Wrapper for the gateway's `POST /v1/events` append-only event log.
//
// Clients batch user-interaction events (scrobble, skip, like, seek) and
// POST them. The recommender folds some of them into per-track preference
// affinity server-side — today only `skip` (with `played_ms`), which tilts
// recommendation relevance away from tracks the user abandons early.
//
// Auth piggybacks on the same Bearer-with-refresh dance as the rest of the
// API client. Copied locally rather than shared because client.ts's
// apiFetch is GET-only and Subsonic-envelope-focused; a future extraction
// is mechanical (see the same note in diagnostics.ts / recommend.ts).

import { refreshTokens } from "../auth/oauth";
import { clearTokens, readTokens } from "../auth/tokens";

class AuthError extends Error {}

async function apiFetch(path: string, init?: RequestInit): Promise<Response> {
  const tokens = readTokens();
  if (!tokens) throw new AuthError("not signed in");
  const doFetch = (token: string) =>
    fetch(path, {
      ...init,
      headers: { ...(init?.headers ?? {}), Authorization: `Bearer ${token}` },
    });

  let res = await doFetch(tokens.accessToken);
  if (res.status === 401) {
    if (!tokens.refreshToken) {
      clearTokens();
      throw new AuthError("guest session expired");
    }
    try {
      await refreshTokens(tokens.refreshToken);
    } catch {
      clearTokens();
      throw new AuthError("session expired");
    }
    const refreshed = readTokens();
    if (!refreshed) throw new AuthError("session expired");
    res = await doFetch(refreshed.accessToken);
    if (res.status === 401) {
      clearTokens();
      throw new AuthError("session expired");
    }
  }
  return res;
}

/** Snake_case to match the gateway's `EventType` serde wire format. */
export type RecommendEventType =
  | "scrobble"
  | "skip"
  | "complete"
  | "like"
  | "unlike"
  | "seek";

export interface RecommendEvent {
  event_type: RecommendEventType;
  track_id: string;
  /** Client-supplied unix milliseconds. */
  occurred_at: number;
  /** Opaque type-specific blob: `{ played_ms, heard_ms, duration_ms,
   *  source }` on a skip / complete. The gateway adds `listen_session`. */
  metadata?: Record<string, unknown>;
  /** Active recommend-session, if any. */
  session_id?: string;
  /** Client-chosen id; a resend with the same id persists once. Set by
   *  the outbox (eventOutbox.ts). */
  client_event_id?: string;
}

// The player doesn't call this directly: it queues through eventOutbox.ts,
// which keeps events until delivery is confirmed. Throws on a non-2xx so
// callers that *do* care (tests) can observe it.
export async function postEvents(events: RecommendEvent[]): Promise<void> {
  if (events.length === 0) return;
  const status = await sendEvents(events);
  if (status < 200 || status >= 300) throw new Error(`events HTTP ${status}`);
}

/** POST a batch and resolve to the HTTP status. Rejects only when the
 *  request didn't complete (no network) or the session is gone. */
export async function sendEvents(events: RecommendEvent[]): Promise<number> {
  const res = await apiFetch("/v1/events", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ events }),
  });
  return res.status;
}

export { AuthError };
