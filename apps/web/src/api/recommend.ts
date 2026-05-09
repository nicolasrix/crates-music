// Wrapper for the gateway's /v1/recommend/* endpoints.
//
// Auth: piggybacks on the Subsonic apiFetch helper in client.ts, which adds
// the Bearer token and handles 401 → refresh → retry. The recommender
// endpoints are *not* Subsonic-shaped (no envelope), so we do raw JSON
// parsing here rather than going through getSubsonic.

import { refreshTokens } from "../auth/oauth";
import { clearTokens, readTokens } from "../auth/tokens";
import { getSong } from "./client";
import type { Track } from "./types";

class AuthError extends Error {}

async function apiFetch(path: string): Promise<Response> {
  const tokens = readTokens();
  if (!tokens) throw new AuthError("not signed in");
  const doFetch = (token: string) =>
    fetch(path, { headers: { Authorization: `Bearer ${token}` } });

  let res = await doFetch(tokens.accessToken);
  if (res.status === 401) {
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

export interface RecommendItem {
  track_id: string;
  similarity: number;
}

export interface RecommendResponse {
  seed: string;
  model_version: string | null;
  degraded: boolean;
  results: RecommendItem[];
}

/** Distinguishes the "seed not in ANN yet" path from a real failure. The
 *  consumer can react with a friendly "this track isn't indexed yet" instead
 *  of a generic error. */
export class SeedNotEmbeddedError extends Error {
  constructor(public seed: string) {
    super(`seed ${seed} not embedded`);
  }
}

export async function fetchRecommendations(
  seed: string,
  n = 20
): Promise<RecommendResponse> {
  const res = await apiFetch(
    `/v1/recommend/next?seed=${encodeURIComponent(seed)}&n=${n}`
  );
  if (res.status === 404) throw new SeedNotEmbeddedError(seed);
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return (await res.json()) as RecommendResponse;
}

/** End-to-end "start station" helper: fetch top-N recommendations for `seed`,
 *  then hydrate each result to a full Track via Subsonic getSong so the
 *  player UI has artist/album/duration to display.
 *
 *  Failures on individual song lookups are tolerated — a track that can't be
 *  resolved is dropped from the station rather than aborting the whole flow. */
export async function startStation(seed: string, n = 20): Promise<Track[]> {
  const rec = await fetchRecommendations(seed, n);
  // Parallel fetch — with HTTP/2 multiplexing the gateway and Navidrome
  // handle this without trouble at N≤20.
  const tracks = await Promise.all(
    rec.results.map((r) => getSong(r.track_id).catch(() => null))
  );
  return tracks.filter((t): t is Track => t !== null);
}

/** Try `startStation` against each candidate seed in order, falling through
 *  SeedNotEmbeddedError until one succeeds. Useful when the caller has
 *  several plausible seeds (an album's tracks, say) and only needs *one* to
 *  be indexed for the station to be meaningful.
 *
 *  Throws SeedNotEmbeddedError on the *last* seed if every candidate is
 *  unindexed. Other errors short-circuit immediately — they likely indicate
 *  a backend problem rather than a missing-embedding case. */
export async function startStationFromAny(
  candidates: readonly string[],
  n = 20
): Promise<{ seed: string; tracks: Track[] }> {
  if (candidates.length === 0) throw new Error("no candidate seeds");
  let lastNotEmbedded: SeedNotEmbeddedError | null = null;
  for (const seed of candidates) {
    try {
      const tracks = await startStation(seed, n);
      return { seed, tracks };
    } catch (e) {
      if (e instanceof SeedNotEmbeddedError) {
        lastNotEmbedded = e;
        continue;
      }
      throw e;
    }
  }
  throw lastNotEmbedded ?? new Error("no candidate seed produced a station");
}
