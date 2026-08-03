// The "most played" ranking rule for an artist page.
//
// Split out from artistTracks so it stays a pure function over types —
// artistTracks imports the API client, which reaches `location` at
// module load and so can't be pulled into the node-environment test
// run. Same split as rowMenuCoords / playbackHelpers.

import type { Track } from "../api/types";

export interface ArtistRef {
  id: string;
  name: string;
}

/** Does this search3 row belong to the artist whose page we're on?
 *
 *  Matching on id alone would be tidier, but a track credited to a
 *  featured or variant-spelled artist can carry a different artistId
 *  while plainly being theirs. Matching on name alone is worse: an
 *  artist's canonical display name is whatever the tags say — one live
 *  library has "MAC MILLER" against tracks tagged "Mac Miller", which
 *  drops ~16% of the catalog on an exact compare. So: id first, then a
 *  case-insensitive name fallback. */
function isBy(track: Track, artist: ArtistRef): boolean {
  if (track.artistId !== undefined && track.artistId === artist.id) return true;
  return (
    track.artist !== undefined &&
    track.artist.toLowerCase() === artist.name.toLowerCase()
  );
}

/** Rank an artist's tracks by our own play count, most-played first.
 *
 *  Zero-play tracks are dropped rather than tailed on: Navidrome omits
 *  `playCount` entirely when it's zero, so "absent" and "never played"
 *  are the same state, and neither belongs under a heading that claims
 *  plays. An artist we've never listened to therefore ranks empty —
 *  which is the signal the page uses to hide the section.
 *
 *  Ties break on title so the order is stable across refetches; without
 *  it, two tracks on equal counts can swap places under the cursor.
 *
 *  Returns a new array — the input is a shared query result. */
export function mostPlayedForArtist(
  tracks: readonly Track[],
  artist: ArtistRef,
  limit: number,
): Track[] {
  return tracks
    .filter((t) => (t.playCount ?? 0) > 0 && isBy(t, artist))
    .sort(
      (a, b) =>
        (b.playCount ?? 0) - (a.playCount ?? 0) ||
        a.title.localeCompare(b.title),
    )
    .slice(0, limit);
}
