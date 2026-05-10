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

export interface PlaylistSuggestionResult {
  tracks: Track[];
  /** True when *every* sampled seed was unindexed. Lets the UI distinguish
   *  "playlist hasn't been embedded yet" from "we just don't have anything
   *  more to suggest" (e.g. tiny library where everything is already in
   *  the playlist). */
  allSeedsUnindexed: boolean;
}

/** Pick suggestions for an existing playlist by aggregating per-seed
 *  recommendations across a sample of the playlist's tracks. We can't
 *  hand the gateway a multi-seed query (no such endpoint), so we
 *  fan out N single-seed queries client-side and combine.
 *
 *  Aggregation: each candidate's score = Σ similarity across seeds that
 *  surfaced it. Tracks that appear under multiple seeds rank higher
 *  than tracks that appear under just one — a cheap centroid proxy.
 *
 *  Sampling: random sample of `sampleSize` seeds (default 8). Random
 *  rather than first-N so we cover the playlist's full vibe instead of
 *  whatever was added earliest.
 *
 *  Exclusions: candidates already in the playlist are filtered out. The
 *  `excludeIds` parameter lets callers also drop tracks they've just
 *  added or dismissed without refetching. */
export async function suggestForPlaylist(
  playlistTrackIds: readonly string[],
  opts: {
    /** Per-seed candidate count. Default 20 — same as the station call. */
    perSeedN?: number;
    /** Max seeds to sample. Default 8. */
    sampleSize?: number;
    /** Final result cap. Default 20. */
    topN?: number;
    /** Extra IDs to exclude from suggestions (e.g. the playlist's own
     *  tracks are added automatically; pass any further "I just added
     *  this" or "dismissed" IDs here). */
    excludeIds?: readonly string[];
  } = {}
): Promise<PlaylistSuggestionResult> {
  const perSeedN = opts.perSeedN ?? 20;
  const sampleSize = opts.sampleSize ?? 8;
  const topN = opts.topN ?? 20;

  if (playlistTrackIds.length === 0) {
    return { tracks: [], allSeedsUnindexed: false };
  }

  const seeds = sampleN(playlistTrackIds, sampleSize);

  // Fan out per-seed queries. Promise.allSettled lets us tolerate
  // per-seed failures — typical case is a few unindexed tracks among
  // mostly-indexed ones, and we want to use whatever we got rather
  // than abort.
  const settled = await Promise.allSettled(
    seeds.map((s) => fetchRecommendations(s, perSeedN))
  );

  let unindexedCount = 0;
  const scores = new Map<string, number>();
  for (const r of settled) {
    if (r.status === "rejected") {
      if (r.reason instanceof SeedNotEmbeddedError) unindexedCount++;
      continue;
    }
    for (const item of r.value.results) {
      scores.set(item.track_id, (scores.get(item.track_id) ?? 0) + item.similarity);
    }
  }

  const allSeedsUnindexed = unindexedCount === seeds.length;

  // Drop any candidate already in the playlist or in the caller's
  // exclude list. Use a Set of strings for O(1) lookup; the playlist
  // can be large.
  const exclude = new Set<string>(playlistTrackIds);
  for (const id of opts.excludeIds ?? []) exclude.add(id);

  const ranked = Array.from(scores.entries())
    .filter(([id]) => !exclude.has(id))
    .sort((a, b) => b[1] - a[1])
    .slice(0, topN)
    .map(([id]) => id);

  // Hydrate to full Track shape so the UI can show artist/album/cover.
  // Drop lookup failures silently — same policy as startStation.
  const tracks = await Promise.all(
    ranked.map((id) => getSong(id).catch(() => null))
  );
  return {
    tracks: tracks.filter((t): t is Track => t !== null),
    allSeedsUnindexed,
  };
}

// Knuth shuffle, truncated to k. Avoids the "sort by random key" trick,
// which is biased on V8 for some array sizes — and we don't care about
// sorting the rest of the array, so partial shuffle is faster anyway.
function sampleN<T>(arr: readonly T[], k: number): T[] {
  if (arr.length <= k) return [...arr];
  const copy = [...arr];
  for (let i = 0; i < k; i++) {
    const j = i + Math.floor(Math.random() * (copy.length - i));
    [copy[i], copy[j]] = [copy[j]!, copy[i]!];
  }
  return copy.slice(0, k);
}
