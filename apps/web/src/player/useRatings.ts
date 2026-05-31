// TanStack-Query-backed access to the gateway's durable per-track
// like/dislike. The QueryClient is already the app-wide shared store, so a
// dedicated React Context would add nothing — `useQuery` on a fixed key
// gives every consumer (the PlayerBar control, the Liked-songs page, the
// auto-skip effect) the same map, and a `useQuery` read inside an effect is
// synchronous against the current cache.
//
// staleTime is short and refetchOnWindowFocus is on so a rating made on
// another device shows up here within a focus cycle. Recommender
// enforcement (dislike-exclude, like-boost) is always server-fresh
// regardless — this cache only drives the UI and the optimistic auto-skip.

import { useCallback } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { getRatings, putRating, type Rating } from "../api/library";

export const RATINGS_KEY = ["library", "ratings"] as const;

/** track_id → verdict. Absence from the map is neutral. */
export type RatingMap = Map<string, "like" | "dislike">;

function toMap(rows: { track_id: string; rating: "like" | "dislike" }[]): RatingMap {
  return new Map(rows.map((r) => [r.track_id, r.rating]));
}

/** The current ratings map, or `undefined` while the first fetch is in
 *  flight (callers should fail *open* on `undefined` — never auto-skip a
 *  track we don't yet know the rating of). */
export function useRatingsMap(): RatingMap | undefined {
  const q = useQuery({
    queryKey: RATINGS_KEY,
    queryFn: async () => toMap(await getRatings()),
    staleTime: 30_000,
    refetchOnWindowFocus: true,
  });
  return q.data;
}

export interface TrackRatingState {
  rating: Rating;
  pending: boolean;
  /** Set a verdict. Passing the *current* verdict toggles it off (clear). */
  set: (next: "like" | "dislike") => void;
}

/** Tri-state rating control for one track, with an optimistic PUT that
 *  patches the shared map immediately and rolls back on failure. */
export function useTrackRating(trackId: string | undefined): TrackRatingState {
  const qc = useQueryClient();
  const map = useRatingsMap();
  const current: Rating = trackId ? (map?.get(trackId) ?? null) : null;

  const mutation = useMutation({
    mutationFn: ({ id, rating }: { id: string; rating: Rating }) =>
      putRating(id, rating),
    onMutate: async ({ id, rating }) => {
      // Cancel in-flight refetches so they don't clobber the optimistic
      // patch, then snapshot for rollback.
      await qc.cancelQueries({ queryKey: RATINGS_KEY });
      const prev = qc.getQueryData<RatingMap>(RATINGS_KEY);
      const next = new Map(prev ?? []);
      if (rating === null) next.delete(id);
      else next.set(id, rating);
      qc.setQueryData(RATINGS_KEY, next);
      return { prev };
    },
    onError: (_err, _vars, ctx) => {
      if (ctx?.prev) qc.setQueryData(RATINGS_KEY, ctx.prev);
    },
    onSettled: () => {
      void qc.invalidateQueries({ queryKey: RATINGS_KEY });
    },
  });

  const { mutate } = mutation;
  const set = useCallback(
    (next: "like" | "dislike") => {
      if (!trackId) return;
      // Toggle-off: clicking the already-active verdict clears it.
      const target: Rating = current === next ? null : next;
      mutate({ id: trackId, rating: target });
    },
    [trackId, current, mutate],
  );

  return { rating: current, pending: mutation.isPending, set };
}
