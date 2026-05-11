// Wrapper for the gateway's /v1/recommend/* endpoints.
//
// Auth: piggybacks on the same Bearer-with-refresh dance the rest of
// the API client uses. The recommender endpoints are *not* Subsonic-
// shaped (no envelope), so we parse raw JSON here rather than going
// through getSubsonic.
//
// Aggregation across seeds (Σ-similarity), random sampling, and the
// first-indexed-wins fallback all live server-side now (gateway
// `/v1/recommend/from-seeds` and `/v1/recommend/from-any`). This module
// is a thin transport layer plus client-side `getSong` hydration so
// callers receive full Track shapes, not just track ids.

import { refreshTokens } from "../auth/oauth";
import { clearTokens, readTokens } from "../auth/tokens";
import { getSong } from "./client";
import type { Track } from "./types";

class AuthError extends Error {}

async function apiFetch(path: string, init?: RequestInit): Promise<Response> {
  const tokens = readTokens();
  if (!tokens) throw new AuthError("not signed in");
  const doFetch = (token: string) =>
    fetch(path, {
      ...init,
      headers: {
        ...(init?.headers ?? {}),
        Authorization: `Bearer ${token}`,
      },
    });

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

async function postJson(path: string, body: unknown): Promise<Response> {
  return apiFetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
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

/** Hydrate a list of track ids to full Track shapes via Subsonic
 *  getSong. Failures on individual lookups are tolerated — a track
 *  that can't be resolved is dropped rather than aborting the flow.
 *  Parallel: HTTP/2 multiplexing handles N≤20 without trouble. */
async function hydrateTracks(ids: readonly string[]): Promise<Track[]> {
  const tracks = await Promise.all(ids.map((id) => getSong(id).catch(() => null)));
  return tracks.filter((t): t is Track => t !== null);
}

/** End-to-end "start station" helper for a single seed: fetch top-N
 *  recommendations, then hydrate to full Track via Subsonic getSong. */
export async function startStation(seed: string, n = 20): Promise<Track[]> {
  const rec = await fetchRecommendations(seed, n);
  return hydrateTracks(rec.results.map((r) => r.track_id));
}

interface FromAnyResponse {
  seed_used: string;
  model_version: string | null;
  degraded: boolean;
  results: RecommendItem[];
}

/** Slate-selection algorithm. Mirrors `DiversityMode` in the gateway —
 *  `hard_cap` is the legacy cap-only behaviour, `mmr` is Maximal
 *  Marginal Relevance over the candidate vectors, `off` disables
 *  diversity gating entirely. Server default is `hard_cap`; the web
 *  client opts into `mmr` from `AutoplayContext`. */
export type DiversityMode = "hard_cap" | "mmr" | "off";

/** Queue snapshot the gateway uses for diversity filtering (artist
 *  cap + cross-edition title dedup). Wire shape mirrors the server's
 *  `QueueContext`. Optional knobs default to the server's own
 *  defaults (`max_per_artist=2`, `dedup_titles=true`,
 *  `diversity_mode="hard_cap"`, `mmr_lambda=0.7`). */
export interface QueueContext {
  queueTrackIds: readonly string[];
  nowPlayingTrackId?: string;
  /** `0` disables the per-artist cap. */
  maxPerArtist?: number;
  /** `false` disables (artist, normalized-title) dedup. */
  dedupTitles?: boolean;
  /** Slate selection algorithm. Omit to use the server default. */
  diversityMode?: DiversityMode;
  /** MMR relevance/novelty tradeoff in [0, 1]. Only consulted when
   *  `diversityMode === "mmr"`. */
  mmrLambda?: number;
}

function serializeQueueContext(qc: QueueContext): Record<string, unknown> {
  const body: Record<string, unknown> = {
    queue_track_ids: [...qc.queueTrackIds],
  };
  if (qc.nowPlayingTrackId) body.now_playing_track_id = qc.nowPlayingTrackId;
  if (qc.maxPerArtist !== undefined) body.max_per_artist = qc.maxPerArtist;
  if (qc.dedupTitles !== undefined) body.dedup_titles = qc.dedupTitles;
  if (qc.diversityMode !== undefined) body.diversity_mode = qc.diversityMode;
  if (qc.mmrLambda !== undefined) body.mmr_lambda = qc.mmrLambda;
  return body;
}

/** Try `candidates` in order, returning the first that has an ANN
 *  entry plus its top-N similar tracks. The seed-by-seed fallthrough
 *  runs server-side now: one POST instead of one GET-per-candidate.
 *
 *  When `queueContext` is supplied, the gateway applies an artist cap
 *  + title dedup against the queue snapshot before returning, so the
 *  caller can blindly enqueue the response without re-filtering.
 *
 *  Throws SeedNotEmbeddedError on the *last* candidate if none are
 *  indexed (mirrors the previous client-side semantics so callers
 *  don't need to change). */
export async function startStationFromAny(
  candidates: readonly string[],
  n = 20,
  queueContext?: QueueContext
): Promise<{ seed: string; tracks: Track[] }> {
  if (candidates.length === 0) throw new Error("no candidate seeds");

  const body: Record<string, unknown> = {
    candidate_seeds: [...candidates],
    n,
  };
  if (queueContext) body.queue_context = serializeQueueContext(queueContext);

  const res = await postJson("/v1/recommend/from-any", body);
  if (res.status === 404) {
    // Mirror the old behavior: throw with the last seed as the
    // "blamed" id so existing UI copy (\"$id isn't indexed yet\") still
    // makes sense to the user.
    throw new SeedNotEmbeddedError(candidates[candidates.length - 1]!);
  }
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  const parsed = (await res.json()) as FromAnyResponse;
  const tracks = await hydrateTracks(parsed.results.map((r) => r.track_id));
  return { seed: parsed.seed_used, tracks };
}

export interface PlaylistSuggestionResult {
  tracks: Track[];
  /** True when *every* sampled seed was unindexed. Lets the UI distinguish
   *  "playlist hasn't been embedded yet" from "we just don't have anything
   *  more to suggest" (e.g. tiny library where everything is already in
   *  the playlist). */
  allSeedsUnindexed: boolean;
}

/** A seed with an explicit weight for `from-seeds` aggregation. The
 *  weight scales the seed's per-hit similarity in the Σ-similarity
 *  fold — use this to bias the recommender toward an anchor (weight
 *  3) over user-picked items (weight 2) over already-played scrobbles
 *  (weight 1). */
export interface WeightedSeed {
  trackId: string;
  weight: number;
}

/** Multi-seed station with per-seed weights. Wraps `from-seeds` and
 *  hydrates the result to full Track objects. Unlike
 *  `startStationFromAny`, this *aggregates* across seeds (Σ-similarity
 *  in the centroid of the seed set) rather than picking the first
 *  indexed seed — better when the seed list reflects the listening
 *  context (anchor + user-picked + scrobbles) rather than a list of
 *  fallback candidates.
 *
 *  When `sessionId` is supplied, the gateway adds tracks downvoted in
 *  that recommend-session to the exclusion set — keeps "I just
 *  thumbs-downed this" tracks from coming back the next refill. */
export async function startWeightedStation(
  seeds: readonly WeightedSeed[],
  n = 20,
  queueContext?: QueueContext,
  sessionId?: string,
): Promise<{ tracks: Track[]; allSeedsUnindexed: boolean }> {
  if (seeds.length === 0) {
    return { tracks: [], allSeedsUnindexed: false };
  }
  const body: Record<string, unknown> = {
    seeds: seeds.map((s) => s.trackId),
    seed_weights: seeds.map((s) => s.weight),
    top_n: n,
  };
  if (queueContext) body.queue_context = serializeQueueContext(queueContext);
  if (sessionId) body.session_id = sessionId;

  const res = await postJson("/v1/recommend/from-seeds", body);
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  const parsed = (await res.json()) as FromSeedsResponse;
  const tracks = await hydrateTracks(parsed.results.map((r) => r.track_id));
  return { tracks, allSeedsUnindexed: parsed.all_seeds_unindexed };
}

interface FromSeedsResponse {
  model_version: string | null;
  degraded: boolean;
  results: RecommendItem[];
  all_seeds_unindexed: boolean;
}

/** Pick suggestions for an existing playlist by aggregating per-seed
 *  recommendations on the gateway. Σ-similarity scoring + random
 *  sampling + exclusion of the playlist's own tracks all happen
 *  server-side; here we just translate options to the request body
 *  and hydrate the result.
 *
 *  Defaults match the previous client-side behavior:
 *  perSeedN=20, sampleSize=8, topN=20.
 *
 *  Note: the gateway always excludes the seed set automatically, so
 *  passing the playlist's tracks in `excludeIds` is unnecessary —
 *  but harmless. The parameter still accepts caller-supplied
 *  exclusions for "I just dismissed this" tracks the gateway can't
 *  know about. */
export async function suggestForPlaylist(
  playlistTrackIds: readonly string[],
  opts: {
    perSeedN?: number;
    sampleSize?: number;
    topN?: number;
    excludeIds?: readonly string[];
  } = {}
): Promise<PlaylistSuggestionResult> {
  if (playlistTrackIds.length === 0) {
    return { tracks: [], allSeedsUnindexed: false };
  }

  const body: Record<string, unknown> = {
    seeds: [...playlistTrackIds],
  };
  if (opts.perSeedN !== undefined) body.per_seed_n = opts.perSeedN;
  if (opts.sampleSize !== undefined) body.sample_size = opts.sampleSize;
  if (opts.topN !== undefined) body.top_n = opts.topN;
  if (opts.excludeIds && opts.excludeIds.length > 0) {
    body.exclude_track_ids = [...opts.excludeIds];
  }

  const res = await postJson("/v1/recommend/from-seeds", body);
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  const parsed = (await res.json()) as FromSeedsResponse;
  const tracks = await hydrateTracks(parsed.results.map((r) => r.track_id));
  return { tracks, allSeedsUnindexed: parsed.all_seeds_unindexed };
}

// --- recommendation feedback (thumb up / down) -----------------------------
//
// Distinct from Subsonic's `starred`: this is a vote on *the recommendation*
// (was it a good fit to play right now), not on the song. The UI surface
// must label the buttons accordingly — see PlayerBar.
//
// One UPSERT row per (track_id, session_id). The session_id is generated
// client-side and persisted in sessionStorage (one id per page load), so
// the user can flip up→down freely without earning duplicate votes.

export type FeedbackVote = "up" | "down" | null;

export interface FeedbackResponse {
  track_id: string;
  up: number;
  down: number;
}

export async function submitFeedback(input: {
  trackId: string;
  vote: FeedbackVote;
  sessionId: string;
}): Promise<FeedbackResponse> {
  const res = await postJson("/v1/recommend/feedback", {
    track_id: input.trackId,
    session_id: input.sessionId,
    vote: input.vote,
    occurred_ms: Date.now(),
  });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return (await res.json()) as FeedbackResponse;
}
