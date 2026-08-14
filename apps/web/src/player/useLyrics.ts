// TanStack-Query access to the gateway's lyrics endpoint, keyed by track.
//
// staleTime is deliberately long: the gateway already owns the real cache
// policy (a hit lives for months, a confirmed absence for a week), so a
// second client-side TTL would only add a redundant round-trip. The
// exceptions are the two failure shapes, which must *not* be treated as
// durable answers — see `retry` below.
//
// The query is network-first with an IndexedDB fallback, not the other way
// round. Reading the offline copy first would be faster but would pin a
// downloaded track to whatever its lyrics were on the day you saved it,
// including a wrong fuzzy match that a later refresh already fixed. Going
// to the network first and falling back only on failure means the stored
// copy is a safety net rather than a second source of truth.

import { useCallback } from "react";
import {
  useMutation,
  useQuery,
  useQueryClient,
  type QueryClient,
} from "@tanstack/react-query";

import {
  getLyrics,
  refreshLyrics,
  LyricsDisabledError,
  type LyricsDoc,
} from "../api/lyrics";
import { getLyricsCache } from "../cache/lyricsCache";

export const lyricsKey = (trackId: string) => ["lyrics", trackId] as const;

/** One hour. Long enough that reopening the panel during a listening
 *  session never refetches; short enough that a refresh triggered on
 *  another device lands the same evening. */
const LYRICS_STALE_MS = 60 * 60_000;

/** What the query holds. The `offline` flag is not part of the wire
 *  document — it records *how* this copy was obtained, so the panel can say
 *  so rather than silently presenting a possibly-stale answer as live. */
interface LyricsResult {
  doc: LyricsDoc;
  offline: boolean;
}

async function fetchLyrics(trackId: string): Promise<LyricsResult> {
  try {
    const doc = await getLyrics(trackId);
    // Keep a downloaded track's offline copy in step with the server's
    // answer. Only rows that already exist are touched, so merely browsing
    // lyrics never creates one — see lyricsCache.ts for why that bound
    // matters.
    void getLyricsCache()
      .refreshIfPresent(doc)
      .catch(() => {});
    return { doc, offline: false };
  } catch (err) {
    const stored = await getLyricsCache()
      .get(trackId)
      .catch(() => null);
    if (stored) return { doc: stored, offline: true };
    throw err;
  }
}

function lyricsQueryOptions(trackId: string) {
  return {
    queryKey: lyricsKey(trackId),
    queryFn: () => fetchLyrics(trackId),
    staleTime: LYRICS_STALE_MS,
  };
}

export interface LyricsState {
  doc: LyricsDoc | undefined;
  /** True when `doc` came from the offline store because the gateway could
   *  not be reached. */
  offline: boolean;
  loading: boolean;
  /** Why the *fetch* failed. Kept separate from `refreshError`: a failed
   *  fetch replaces the panel's contents, a failed refresh only warrants a
   *  toast over lyrics that are still on screen. */
  error: Error | null;
  /** Re-run the fetch after a transient failure. */
  retry: () => void;
  /** Re-resolve server-side, discarding the cached answer. */
  refresh: () => void;
  refreshing: boolean;
  refreshError: Error | null;
}

export function useLyrics(trackId: string | undefined): LyricsState {
  const qc = useQueryClient();

  const query = useQuery({
    ...lyricsQueryOptions(trackId ?? ""),
    enabled: Boolean(trackId),
    // A disabled gateway is a configuration fact, not a blip — retrying
    // three times just delays the message. Everything else (including a
    // 503 from an unreachable provider) is worth one more attempt.
    retry: (attempt, error) => !(error instanceof LyricsDisabledError) && attempt < 1,
  });

  const mutation = useMutation({
    mutationFn: () => refreshLyrics(trackId!),
    // Seed the cache from the response rather than invalidating: the POST
    // already returned the freshly-resolved document, so a follow-up GET
    // would fetch the same bytes we are holding. A refresh always speaks to
    // the gateway, so whatever it returns is by definition not offline.
    onSuccess: (doc) => {
      if (!trackId) return;
      qc.setQueryData<LyricsResult>(lyricsKey(trackId), { doc, offline: false });
      void getLyricsCache()
        .refreshIfPresent(doc)
        .catch(() => {});
    },
  });

  const { mutate } = mutation;
  const refresh = useCallback(() => {
    if (trackId) mutate();
  }, [trackId, mutate]);

  const { refetch } = query;
  const retry = useCallback(() => {
    void refetch();
  }, [refetch]);

  return {
    doc: query.data?.doc,
    offline: query.data?.offline ?? false,
    loading: query.isPending && Boolean(trackId),
    error: (query.error as Error | null) ?? null,
    retry,
    refresh,
    refreshing: mutation.isPending,
    refreshError: (mutation.error as Error | null) ?? null,
  };
}

/**
 * Warm one track's lyrics into the query cache ahead of time.
 *
 * Called only while the panel is open, which is the whole design: a blanket
 * prefetch on every track change would send the artist and title of
 * everything played to lrclib.net, for tracks nobody asked to read. Gating
 * it on the panel being visible keeps egress proportional to intent and
 * still makes the case that matters — a track ending while you are reading
 * along — instant at the boundary.
 */
export function prefetchLyrics(qc: QueryClient, trackId: string | undefined): void {
  if (!trackId) return;
  void qc.prefetchQuery(lyricsQueryOptions(trackId)).catch(() => {});
}
