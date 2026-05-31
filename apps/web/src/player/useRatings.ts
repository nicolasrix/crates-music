// TanStack-Query-backed access to the gateway's durable like/dislike for
// tracks, albums, and artists. The QueryClient is already the app-wide
// shared store, so a dedicated React Context would add nothing — `useQuery`
// on a fixed key gives every consumer (the rating controls, the Liked page,
// the auto-skip effect) the same maps, and a `useQuery` read inside an
// effect is synchronous against the current cache.
//
// staleTime is short and refetchOnWindowFocus is on so a rating made on
// another device shows up here within a focus cycle. Recommender
// enforcement (dislike-exclude, like-boost) is always server-fresh
// regardless — this cache only drives the UI and the optimistic auto-skip.

import { useCallback } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  getRatings,
  putRating,
  type EntityKind,
  type Rating,
  type RatingRow,
} from "../api/library";

export const RATINGS_KEY = ["library", "ratings"] as const;

/** entity id → verdict. Absence from the map is neutral. */
export type RatingMap = Map<string, "like" | "dislike">;

/** The ratings split by kind. One fetch, three lookup maps. */
export interface RatingMaps {
  tracks: RatingMap;
  albums: RatingMap;
  artists: RatingMap;
}

function toMaps(rows: RatingRow[]): RatingMaps {
  const maps: RatingMaps = {
    tracks: new Map(),
    albums: new Map(),
    artists: new Map(),
  };
  for (const r of rows) {
    if (r.kind === "track") maps.tracks.set(r.id, r.rating);
    else if (r.kind === "album") maps.albums.set(r.id, r.rating);
    else maps.artists.set(r.id, r.rating);
  }
  return maps;
}

function mapForKind(maps: RatingMaps, kind: EntityKind): RatingMap {
  return kind === "track" ? maps.tracks : kind === "album" ? maps.albums : maps.artists;
}

/** The current ratings maps, or `undefined` while the first fetch is in
 *  flight (callers should fail *open* on `undefined` — never auto-skip a
 *  track we don't yet know the rating of). */
export function useRatingsMaps(): RatingMaps | undefined {
  const q = useQuery({
    queryKey: RATINGS_KEY,
    queryFn: async () => toMaps(await getRatings()),
    staleTime: 30_000,
    refetchOnWindowFocus: true,
  });
  return q.data;
}

export interface EntityRatingState {
  rating: Rating;
  pending: boolean;
  /** Set a verdict. Passing the *current* verdict toggles it off (clear). */
  set: (next: "like" | "dislike") => void;
}

/** Tri-state rating control for one entity (track / album / artist), with an
 *  optimistic PUT that patches the shared maps immediately and rolls back on
 *  failure. */
export function useEntityRating(
  kind: EntityKind,
  id: string | undefined,
): EntityRatingState {
  const qc = useQueryClient();
  const maps = useRatingsMaps();
  const current: Rating = id ? (maps && mapForKind(maps, kind).get(id)) ?? null : null;

  const mutation = useMutation({
    mutationFn: ({ entityId, rating }: { entityId: string; rating: Rating }) =>
      putRating(kind, entityId, rating),
    onMutate: async ({ entityId, rating }) => {
      // Cancel in-flight refetches so they don't clobber the optimistic
      // patch, then snapshot for rollback.
      await qc.cancelQueries({ queryKey: RATINGS_KEY });
      const prev = qc.getQueryData<RatingMaps>(RATINGS_KEY);
      const next: RatingMaps = {
        tracks: new Map(prev?.tracks ?? []),
        albums: new Map(prev?.albums ?? []),
        artists: new Map(prev?.artists ?? []),
      };
      const target = mapForKind(next, kind);
      if (rating === null) target.delete(entityId);
      else target.set(entityId, rating);
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
      if (!id) return;
      // Toggle-off: clicking the already-active verdict clears it.
      const target: Rating = current === next ? null : next;
      mutate({ entityId: id, rating: target });
    },
    [id, current, mutate],
  );

  return { rating: current, pending: mutation.isPending, set };
}
