// TanStack-Query access to the gateway's lyrics endpoint, keyed by track.
//
// staleTime is deliberately long: the gateway already owns the real cache
// policy (a hit lives for months, a confirmed absence for a week), so a
// second client-side TTL would only add a redundant round-trip. The
// exceptions are the two failure shapes, which must *not* be treated as
// durable answers — see `retry` below.

import { useCallback } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  getLyrics,
  refreshLyrics,
  LyricsDisabledError,
  type LyricsDoc,
} from "../api/lyrics";

export const lyricsKey = (trackId: string) => ["lyrics", trackId] as const;

/** One hour. Long enough that reopening the panel during a listening
 *  session never refetches; short enough that a refresh triggered on
 *  another device lands the same evening. */
const LYRICS_STALE_MS = 60 * 60_000;

export interface LyricsState {
  doc: LyricsDoc | undefined;
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
    queryKey: lyricsKey(trackId ?? ""),
    enabled: Boolean(trackId),
    queryFn: () => getLyrics(trackId!),
    staleTime: LYRICS_STALE_MS,
    // A disabled gateway is a configuration fact, not a blip — retrying
    // three times just delays the message. Everything else (including a
    // 503 from an unreachable provider) is worth one more attempt.
    retry: (attempt, error) => !(error instanceof LyricsDisabledError) && attempt < 1,
  });

  const mutation = useMutation({
    mutationFn: () => refreshLyrics(trackId!),
    // Seed the cache from the response rather than invalidating: the POST
    // already returned the freshly-resolved document, so a follow-up GET
    // would fetch the same bytes we are holding.
    onSuccess: (doc) => {
      if (trackId) qc.setQueryData(lyricsKey(trackId), doc);
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
    doc: query.data,
    loading: query.isPending && Boolean(trackId),
    error: (query.error as Error | null) ?? null,
    retry,
    refresh,
    refreshing: mutation.isPending,
    refreshError: (mutation.error as Error | null) ?? null,
  };
}
