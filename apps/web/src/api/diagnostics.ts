// Fetchers for the gateway's /v1/diagnostics/* endpoints. Returns
// already-shaped data (no envelope unwrap). Auth is handled by the
// shared apiFetch helper — copied locally because the existing one in
// client.ts doesn't export apiFetch and is currently focused on the
// Subsonic envelope. We keep this module standalone so a future
// extraction is mechanical.

import { refreshTokens } from "../auth/oauth";
import { clearTokens, readTokens } from "../auth/tokens";

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

async function getJson<T>(path: string): Promise<T> {
  const res = await apiFetch(path);
  if (!res.ok) throw new Error(`HTTP ${res.status} from ${path}`);
  return (await res.json()) as T;
}

async function postJson<T>(path: string): Promise<T> {
  const tokens = readTokens();
  if (!tokens) throw new AuthError("not signed in");
  const res = await fetch(path, {
    method: "POST",
    headers: { Authorization: `Bearer ${tokens.accessToken}` },
  });
  if (!res.ok) throw new Error(`HTTP ${res.status} from ${path}`);
  return (await res.json()) as T;
}

export interface InvalidateCacheResponse {
  removed: number;
}

/** Flush the gateway's L2 browse cache. Returns the row count removed. */
export function invalidateBrowseCache(): Promise<InvalidateCacheResponse> {
  return postJson<InvalidateCacheResponse>("/v1/admin/cache/invalidate");
}

export interface TraceEntry {
  trace_id: string;
  span_id: number;
  parent_span_id: number | null;
  name: string;
  target: string;
  start_ms: number;
  end_ms: number;
  duration_ms: number;
  // Free-form: the gateway parses fields_json server-side, falling back
  // to {"_raw": "..."} on parse failure.
  fields: Record<string, unknown>;
}

export interface TracesResponse {
  traces: TraceEntry[];
}

export interface HistogramBucket {
  name: string;
  count: number;
  min_ms: number;
  max_ms: number;
  p50_ms: number;
  p95_ms: number;
  p99_ms: number;
  mean_ms: number;
}

export interface HistogramResponse {
  buckets: HistogramBucket[];
}

export interface QueueDepthResponse {
  model_version: string;
  not_started: number;
  in_progress: number;
  done: number;
  failed: number;
}

export function fetchTraces(opts: {
  limit?: number;
  name?: string;
  sinceMs?: number;
}): Promise<TracesResponse> {
  const qs = new URLSearchParams();
  if (opts.limit !== undefined) qs.set("limit", String(opts.limit));
  if (opts.name) qs.set("name", opts.name);
  if (opts.sinceMs !== undefined) qs.set("since_ms", String(opts.sinceMs));
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return getJson<TracesResponse>(`/v1/diagnostics/traces${suffix}`);
}

export function fetchHistogram(opts: { sinceMs?: number }): Promise<HistogramResponse> {
  const qs = new URLSearchParams();
  if (opts.sinceMs !== undefined) qs.set("since_ms", String(opts.sinceMs));
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return getJson<HistogramResponse>(`/v1/diagnostics/histogram${suffix}`);
}

// --- span_series ----------------------------------------------------------
//
// Time-series of `(end_ms, duration_ms)` for a single span name. Powers
// the per-row plot when a histogram row is expanded on /diagnostics/tracing.

export interface SpanSeriesPoint {
  end_ms: number;
  duration_ms: number;
}

export interface SpanSeriesResponse {
  name: string;
  points: SpanSeriesPoint[];
}

export function fetchSpanSeries(opts: {
  name: string;
  sinceMs?: number;
  limit?: number;
}): Promise<SpanSeriesResponse> {
  const qs = new URLSearchParams();
  qs.set("name", opts.name);
  if (opts.sinceMs !== undefined) qs.set("since_ms", String(opts.sinceMs));
  if (opts.limit !== undefined) qs.set("limit", String(opts.limit));
  return getJson<SpanSeriesResponse>(`/v1/diagnostics/span_series?${qs.toString()}`);
}

// --- span_children: parent → direct-child wall-time breakdown ------------

export interface SpanChildAgg {
  name: string;
  count: number;
  sum_ms: number;
  mean_ms: number;
}

export interface SpanChildrenResponse {
  parent_name: string;
  parent_count: number;
  parent_sum_ms: number;
  children: SpanChildAgg[];
}

export function fetchSpanChildren(opts: {
  name: string;
  sinceMs?: number;
}): Promise<SpanChildrenResponse> {
  const qs = new URLSearchParams();
  qs.set("name", opts.name);
  if (opts.sinceMs !== undefined) qs.set("since_ms", String(opts.sinceMs));
  return getJson<SpanChildrenResponse>(`/v1/diagnostics/span_children?${qs.toString()}`);
}

export function fetchQueueDepth(opts: { modelVersion?: string }): Promise<QueueDepthResponse> {
  const qs = new URLSearchParams();
  if (opts.modelVersion) qs.set("model_version", opts.modelVersion);
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return getJson<QueueDepthResponse>(`/v1/diagnostics/queue_depth${suffix}`);
}

// --- client events --------------------------------------------------------

export interface ClientEventEntry {
  received_ms: number;
  occurred_ms: number;
  session_id: string;
  name: string;
  value_ms: number | null;
  rating: "good" | "needs-improvement" | "poor" | null;
  page_path: string;
  user_agent: string | null;
  fields: Record<string, unknown>;
}

export interface ClientEventsResponse {
  events: ClientEventEntry[];
}

export function fetchClientEvents(opts: {
  limit?: number;
  name?: string;
}): Promise<ClientEventsResponse> {
  const qs = new URLSearchParams();
  if (opts.limit !== undefined) qs.set("limit", String(opts.limit));
  if (opts.name) qs.set("name", opts.name);
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return getJson<ClientEventsResponse>(`/v1/diagnostics/client_events${suffix}`);
}

// --- recently played ------------------------------------------------------

export interface RecentlyPlayedEntry {
  track_id: string;
  /** Client-supplied scrobble timestamp (unix-ms). */
  occurred_at_ms: number;
  /** Gateway-stamped persist time (unix-ms). */
  received_at_ms: number;
  /** All metadata fields are nullable: a track that has been scrobbled
   *  but not yet ingested by the recommender (so it's missing from the
   *  `track_metadata` cache) will return null for every field below.
   *  The UI falls back to the raw `track_id`. */
  title: string | null;
  artist: string | null;
  artist_id: string | null;
  album: string | null;
  album_id: string | null;
  year: number | null;
}

export interface RecentlyPlayedResponse {
  events: RecentlyPlayedEntry[];
}

export function fetchRecentlyPlayed(opts: {
  limit?: number;
  sinceMs?: number;
}): Promise<RecentlyPlayedResponse> {
  const qs = new URLSearchParams();
  if (opts.limit !== undefined) qs.set("limit", String(opts.limit));
  if (opts.sinceMs !== undefined) qs.set("since_ms", String(opts.sinceMs));
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return getJson<RecentlyPlayedResponse>(`/v1/diagnostics/recently_played${suffix}`);
}

// --- recommender aggregates ----------------------------------------------
//
// The four /v1/diagnostics/recommend/* aggregations all key off the same
// recommend.from_any / recommend.from_seeds spans, but slice differently:
// fill-ratio histogram, shortfall reason counts, admitted-similarity
// quantiles, and a recommend-frequency leaderboard.

export interface QueueFillBucket {
  label: string;
  count: number;
}

export interface QueueFillResponse {
  total: number;
  buckets: QueueFillBucket[];
}

export interface ShortfallResponse {
  total: number;
  /** Map of `shortfall_reason` → count. Pre-R1 spans land under `unknown`. */
  counts: Record<string, number>;
}

export interface SimilarityResponse {
  count: number;
  p50: number;
  p90: number;
  p95: number;
  p99: number;
  min: number;
  max: number;
  mean: number;
}

export interface TopResultItem {
  track_id: string;
  count: number;
  title: string | null;
  artist: string | null;
  album: string | null;
}

export interface TopResultsResponse {
  items: TopResultItem[];
}

export function fetchRecommendQueueFill(opts: {
  sinceMs?: number;
}): Promise<QueueFillResponse> {
  const qs = new URLSearchParams();
  if (opts.sinceMs !== undefined) qs.set("since_ms", String(opts.sinceMs));
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return getJson<QueueFillResponse>(`/v1/diagnostics/recommend/queue_fill${suffix}`);
}

export function fetchRecommendShortfall(opts: {
  sinceMs?: number;
}): Promise<ShortfallResponse> {
  const qs = new URLSearchParams();
  if (opts.sinceMs !== undefined) qs.set("since_ms", String(opts.sinceMs));
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return getJson<ShortfallResponse>(`/v1/diagnostics/recommend/shortfall${suffix}`);
}

export function fetchRecommendSimilarity(opts: {
  sinceMs?: number;
}): Promise<SimilarityResponse> {
  const qs = new URLSearchParams();
  if (opts.sinceMs !== undefined) qs.set("since_ms", String(opts.sinceMs));
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return getJson<SimilarityResponse>(`/v1/diagnostics/recommend/similarity${suffix}`);
}

export function fetchRecommendTopResults(opts: {
  limit?: number;
  sinceMs?: number;
}): Promise<TopResultsResponse> {
  const qs = new URLSearchParams();
  if (opts.limit !== undefined) qs.set("limit", String(opts.limit));
  if (opts.sinceMs !== undefined) qs.set("since_ms", String(opts.sinceMs));
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return getJson<TopResultsResponse>(
    `/v1/diagnostics/recommend/top_results${suffix}`
  );
}

// --- latent space (UMAP scatter) -----------------------------------------

export interface LatentSpacePoint {
  track_id: string;
  x: number;
  y: number;
  title: string | null;
  artist: string | null;
  album: string | null;
  /** Reported genre from Subsonic metadata; null when the track has no
   *  genre tag, or no metadata row in the gateway cache yet. The scatter
   *  uses this to colour clusters as a validation of the embedding. */
  genre: string | null;
  /** First four PCA components on the original 512-D CLAP space,
   *  computed by the reducer alongside (x, y). Null per-component on
   *  projections that predate migration 0009 or for components past
   *  the dataset's natural rank. Used by the "colour by → PCn" mode. */
  pc1: number | null;
  pc2: number | null;
  pc3: number | null;
  pc4: number | null;
  /** Third UMAP axis from an `n_components=3` reducer run (migration
   *  0010). Null on 2-D projections. Drives the vertical spatial axis
   *  in the 3-D scene; not exposed as a colour channel since spatial
   *  position already encodes it. */
  z: number | null;
}

export interface LatentSpaceVersionEntry {
  proj_version: string;
  point_count: number;
  created_at_ms: number;
}

export interface LatentSpaceResponse {
  model_version: string;
  /** null when no projection has been written yet for `model_version`. */
  proj_version: string | null;
  points: LatentSpacePoint[];
  versions: LatentSpaceVersionEntry[];
}

export function fetchRecommendLatentSpace(opts: {
  projVersion?: string;
  modelVersion?: string;
  /** Layout preference, server-resolved to the newest matching
   *  projection. Ignored when `projVersion` is explicitly set. */
  prefer?: "2d" | "3d";
}): Promise<LatentSpaceResponse> {
  const qs = new URLSearchParams();
  if (opts.projVersion) qs.set("proj_version", opts.projVersion);
  if (opts.modelVersion) qs.set("model_version", opts.modelVersion);
  if (opts.prefer) qs.set("prefer", opts.prefer);
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return getJson<LatentSpaceResponse>(
    `/v1/diagnostics/recommend/latent_space${suffix}`
  );
}

// --- latent neighbours (hover overlay) -----------------------------------
//
// Returns the k nearest neighbours of a seed track in the original
// CLAP embedding space — the ground-truth distances UMAP can't
// preserve in 2D. The web client fetches this on hover (debounced)
// and uses it to draw ring + connecting-line overlays on the scatter.

export interface LatentNeighbourEntry {
  track_id: string;
  /** Cosine distance in CLAP space (`1 - cosine_similarity`). Range
   *  `[0, 2]`; 0 = identical direction, 1 = orthogonal. */
  cosine_distance: number;
}

export interface LatentNeighboursResponse {
  /** Echoed back so a stale fetch can be detected by the caller. */
  track_id: string;
  /** Ascending by `cosine_distance`. Seed is filtered out server-side. */
  neighbours: LatentNeighbourEntry[];
}

export function fetchRecommendLatentNeighbours(opts: {
  trackId: string;
  k?: number;
}): Promise<LatentNeighboursResponse> {
  const qs = new URLSearchParams();
  qs.set("track_id", opts.trackId);
  if (opts.k !== undefined) qs.set("k", String(opts.k));
  return getJson<LatentNeighboursResponse>(
    `/v1/diagnostics/recommend/latent_neighbours?${qs.toString()}`,
  );
}

// --- recommend sessions --------------------------------------------------

export interface SessionEvent {
  track_id: string;
  event_type: string;
  occurred_at_ms: number;
}

export interface SessionSegment {
  /** Cosine distance between this event's track and the next event's
   *  track in the recommender's CLAP embedding space. `null` when one
   *  of the tracks lacks a `done` embedding under the active model. */
  cosine_distance: number | null;
}

export interface SessionItem {
  session_id: string;
  anchor_track_id: string;
  items_count: number;
  started_ms: number;
  /** null while the session is still active. */
  ended_ms: number | null;
  event_count: number;
  /** Present only when `includeEvents=true`. Oldest-first. */
  events?: SessionEvent[];
  /** Present only when `includeEvents=true`. Length = events.length - 1. */
  segments?: SessionSegment[];
}

export interface SessionsListResponse {
  items: SessionItem[];
}

export function fetchRecommendSessions(opts: {
  limit?: number;
  includeEvents?: boolean;
  modelVersion?: string;
}): Promise<SessionsListResponse> {
  const qs = new URLSearchParams();
  if (opts.limit !== undefined) qs.set("limit", String(opts.limit));
  if (opts.includeEvents) qs.set("include_events", "1");
  if (opts.modelVersion) qs.set("model_version", opts.modelVersion);
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return getJson<SessionsListResponse>(
    `/v1/diagnostics/recommend/sessions${suffix}`
  );
}
