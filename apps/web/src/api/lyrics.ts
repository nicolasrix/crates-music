// Wrapper for the gateway's lyrics surface:
//   GET  /v1/lyrics/:trackId          — resolved lyrics (cached server-side)
//   POST /v1/lyrics/:trackId/refresh  — drop the cached answer, resolve again
//
// The gateway normalizes every source to the same shape, so this client
// never parses LRC or talks to a lyrics provider — it receives lines that
// are already split and timed.
//
// Three outcomes have to stay distinguishable, because they mean very
// different things to the panel:
//   * a document with lines/plain  → show it
//   * a document with source "none" → the track has *confirmed* no lyrics
//   * LyricsUnavailableError (503)  → we could not check right now
// Collapsing the last two would render a transient outage as a permanent
// absence, which is exactly the mistake the gateway's status codes exist
// to prevent.
//
// Auth piggybacks on the same Bearer-with-refresh dance as library.ts /
// events.ts, copied locally for the reason noted there (client.ts's
// apiFetch is GET-only and Subsonic-envelope-focused).

import { refreshTokens } from "../auth/oauth";
import { clearTokens, readTokens } from "../auth/tokens";

class AuthError extends Error {}

/** No lyrics source could be reached (gateway 503). Distinct from an
 *  answered "this track has none" — retrying later may succeed. */
export class LyricsUnavailableError extends Error {}

/** The gateway has lyrics turned off entirely (404). Terminal: retrying
 *  will not help until an admin changes the config. */
export class LyricsDisabledError extends Error {}

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

/** Where the gateway found this answer. `"none"` is a cached, confirmed
 *  absence — not an error. */
export type LyricsSource = "navidrome" | "lrclib" | "none";

/** How closely the provider lookup matched, when one was used. `"search"`
 *  is the fuzziest tier and the one most worth offering a refresh for. */
export type LyricsMatchKind = "exact" | "no_album" | "search" | null;

/** One timed line. `start_ms` is relative to the start of the track and
 *  already carries any provider-supplied offset. */
export interface LyricLine {
  start_ms: number;
  text: string;
}

export interface LyricsDoc {
  track_id: string;
  source: LyricsSource;
  match_kind: LyricsMatchKind;
  /** True when `lines` carries usable timings. */
  synced: boolean;
  instrumental: boolean;
  /** `null` (not `[]`) when unsynced, so "no timings" can't be mistaken
   *  for "timings, but empty". */
  lines: LyricLine[] | null;
  plain: string | null;
  provider_id: string | null;
  fetched_at: number;
}

function mapStatus(res: Response): never {
  if (res.status === 503) {
    throw new LyricsUnavailableError("no lyrics source reachable");
  }
  if (res.status === 404) {
    throw new LyricsDisabledError("lyrics are disabled on this gateway");
  }
  throw new Error(`lyrics HTTP ${res.status}`);
}

/** Fetch lyrics for one track. Resolves for a confirmed absence too — the
 *  caller reads `source === "none"`. */
export async function getLyrics(trackId: string): Promise<LyricsDoc> {
  const res = await apiFetch(`/v1/lyrics/${encodeURIComponent(trackId)}`);
  if (!res.ok) mapStatus(res);
  return (await res.json()) as LyricsDoc;
}

/** Re-resolve, discarding whatever was cached. The escape hatch when a
 *  fuzzy match landed on the wrong song. Guests get 403 here. */
export async function refreshLyrics(trackId: string): Promise<LyricsDoc> {
  const res = await apiFetch(`/v1/lyrics/${encodeURIComponent(trackId)}/refresh`, {
    method: "POST",
  });
  if (res.status === 403) throw new Error("guests cannot refresh lyrics");
  if (!res.ok) mapStatus(res);
  return (await res.json()) as LyricsDoc;
}

export { AuthError };
