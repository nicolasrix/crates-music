// "Everything by this artist, in a sensible play order."
//
// Two sources, in priority order:
//   1. getTopSongs — Navidrome backs this with Last.fm's top-tracks
//      chart mapped onto local files, so the artist's best-known songs
//      lead. (It is NOT play-count-backed, despite what this comment
//      claimed until 2026-08-03 — its rows mostly carry no playCount at
//      all. For *our* counts see sync/mostPlayed + searchArtistSongs.)
//      Still the right opener for a play button: "start with the hits"
//      is what that button means.
//   2. The discography in album order — the fallback for an obscure
//      artist Last.fm has no chart for, where top-songs comes back
//      empty.
//
// Lives here rather than on the Artist page because three surfaces now
// need it (the artist hero's play button, and the ⋯ menus on artist rows
// and hero cards), and the codebase has been bitten before by each page
// growing its own copy of a playback rule — that's why playbackHelpers
// exists.

import type { QueryClient } from "@tanstack/react-query";
import {
  getAlbum,
  getArtist,
  getTopSongs,
  searchArtistSongs,
} from "../api/client";
import type { Album, Track } from "../api/types";

/** Album-order fallback is bounded so a 50-album discography doesn't fan
 *  out 50 getAlbum calls from one click. */
export const PLAY_FALLBACK_ALBUMS = 10;

const ALBUM_STALE_MS = 5 * 60_000;
const TOP_SONGS_STALE_MS = 5 * 60_000;

/** One artist's top songs, as query options.
 *
 *  Two call sites share this entry: the Artist page's "most played"
 *  section (declarative, via useQuery) and the play buttons (imperative,
 *  via fetchTopSongs). They *must* agree on the key so whichever runs
 *  first pays for the other — a second round-trip on the button path can
 *  outlive the click's user activation and break `audio.play()`.
 *
 *  Expressed as a factory rather than an exported key so the two can't
 *  drift, and — load-bearing — so the useQuery consumer supplies the
 *  fetch itself. Routing it through fetchTopSongs instead would have it
 *  await its own in-flight promise and hang forever. */
export function topSongsQuery(artistName: string) {
  return {
    queryKey: ["top-songs", artistName] as const,
    queryFn: () => getTopSongs(artistName),
    staleTime: TOP_SONGS_STALE_MS,
  };
}

/** getTopSongs through the query cache, for callers outside React's
 *  render cycle. Failures resolve to `[]` rather than throwing: every
 *  caller has a fallback, and none wants to surface "Navidrome's
 *  top-songs index is unhappy" to the user. */
export function fetchTopSongs(
  queryClient: QueryClient,
  artistName: string,
): Promise<Track[]> {
  return queryClient.fetchQuery(topSongsQuery(artistName)).catch(() => []);
}

/** The artist's catalog with our play counts, as query options — the
 *  input to the Artist page's "most played" section.
 *
 *  Separate cache entry from topSongsQuery on purpose: the two answer
 *  different questions (our plays vs Last.fm's chart) off different
 *  endpoints, and only this one carries playCount. */
export function artistSongsQuery(artistName: string) {
  return {
    queryKey: ["artist-songs", artistName] as const,
    queryFn: () => searchArtistSongs(artistName),
    staleTime: TOP_SONGS_STALE_MS,
  };
}

/** Re-exported so the Artist page has one import for "artist tracklist
 *  rules"; the implementation lives in ./mostPlayed to stay free of this
 *  module's API-client import. */
export { mostPlayedForArtist } from "./mostPlayed";

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
  const top = await fetchTopSongs(queryClient, artistName);
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
  const top = await fetchTopSongs(queryClient, artist.name);
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
