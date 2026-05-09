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
