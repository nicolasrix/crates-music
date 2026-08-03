// "Everything by this artist, in a sensible play order."
//
// Two sources, in priority order:
//   1. getTopSongs — play-count-backed, so the artist's actual hits lead.
//   2. The discography in album order — the fallback for a never-played
//      artist, where top-songs comes back empty.
//
// Lives here rather than on the Artist page because three surfaces now
// need it (the artist hero's play button, and the ⋯ menus on artist rows
// and hero cards), and the codebase has been bitten before by each page
// growing its own copy of a playback rule — that's why playbackHelpers
// exists.

import type { QueryClient } from "@tanstack/react-query";
import { getAlbum, getArtist, getTopSongs } from "../api/client";
import type { Album, Track } from "../api/types";

/** Album-order fallback is bounded so a 50-album discography doesn't fan
 *  out 50 getAlbum calls from one click. */
export const PLAY_FALLBACK_ALBUMS = 10;

const ALBUM_STALE_MS = 5 * 60_000;

/** Flatten a bounded prefix of a discography into a tracklist. Individual
 *  album fetches that fail are dropped rather than failing the whole
 *  action — a partial queue beats a dead button. */
async function tracksFromAlbums(
  queryClient: QueryClient,
  albums: readonly Album[],
): Promise<Track[]> {
  const details = await Promise.all(
    albums.slice(0, PLAY_FALLBACK_ALBUMS).map((a) =>
      queryClient
        .fetchQuery({
          queryKey: ["album", a.id],
          queryFn: () => getAlbum(a.id),
          staleTime: ALBUM_STALE_MS,
        })
        .catch(() => null),
    ),
  );
  return details.flatMap((d) => d?.tracks ?? []);
}

/** Resolve an artist's tracks when the caller already holds the
 *  discography (the Artist page). */
export async function artistTracksFrom(
  queryClient: QueryClient,
  artistName: string,
  albums: readonly Album[],
): Promise<Track[]> {
  // getTopSongs failures degrade to the album fallback rather than
  // surfacing — the user asked to play something, not to hear about
  // Navidrome's top-songs index.
  const top = await getTopSongs(artistName).catch(() => []);
  if (top.length > 0) return top;
  return tracksFromAlbums(queryClient, albums);
}

/** Resolve an artist's tracks from an id alone (the row menus). The
 *  `["artist", id]` key is the Artist page's own, so navigating there
 *  first makes this a cache hit. */
export async function artistTracks(
  queryClient: QueryClient,
  artist: { id: string; name: string },
): Promise<Track[]> {
  const top = await getTopSongs(artist.name).catch(() => []);
  if (top.length > 0) return top;
  const detail = await queryClient
    .fetchQuery({
      queryKey: ["artist", artist.id],
      queryFn: () => getArtist(artist.id),
      staleTime: ALBUM_STALE_MS,
    })
    .catch(() => null);
  if (!detail) return [];
  return tracksFromAlbums(queryClient, detail.albums);
}
