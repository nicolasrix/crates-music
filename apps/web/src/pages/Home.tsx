// Home — three "recently added" rails: albums, artists, tracks.
//
// Subsonic gives us "recently added albums" directly via type=newest. There's
// no first-class recently-added-artists endpoint, so we derive it from the
// same albums query: take artistId off the newest 60 albums, dedupe in
// insertion order, slice. That's "artists with the freshest releases", which
// is the right reading of "recently added artists" in a single-user library.

import { useQuery } from "@tanstack/react-query";
import { listAlbums, listArtists, listRecentTracks } from "../api/client";
import { AlbumCard } from "../components/AlbumCard";
import { ArtistCard } from "../components/ArtistCard";
import { Layout } from "../components/Layout";
import { Link } from "../router";
import { TrackTable } from "../components/TrackTable";
import { useSync } from "../sync/SyncContext";
import { playSingle } from "../sync/playbackHelpers";
import type { Album, Artist } from "../api/types";

const ALBUMS_N = 12;
const ARTISTS_N = 12;
const TRACKS_N = 20;

export function Home() {
  // One wider album fetch feeds both the albums rail and the artist
  // derivation. Cached separately from Albums.tsx (different size) but
  // TanStack Query will share between visits.
  const albumsQ = useQuery({
    queryKey: ["albums", "newest", 60],
    queryFn: () => listAlbums({ type: "newest", size: 60 }),
    staleTime: 60_000,
  });
  // Canonical artists list — used solely to look up coverArt for the
  // artists derived from recent albums. Album rows don't carry an
  // artistImage / artist coverArt, so without this lookup the artist
  // tiles would render as bare placeholder circles.
  const artistsQ = useQuery({
    queryKey: ["artists"],
    queryFn: listArtists,
    staleTime: 5 * 60_000,
  });
  const tracksQ = useQuery({
    queryKey: ["tracks", "recent", TRACKS_N],
    queryFn: () => listRecentTracks(TRACKS_N),
    staleTime: 60_000,
  });
  const sync = useSync();

  const albums = albumsQ.data ?? [];
  const recentArtists = deriveRecentArtists(albums, artistsQ.data ?? [], ARTISTS_N);

  return (
    <Layout breadcrumb="home">
      <div className="section">
        <div className="section-head">
          <h2>recently added albums</h2>
          <Link to="/albums" className="section-more">
            all albums
          </Link>
        </div>
        {albumsQ.isLoading && (
          <p className="text-fg-muted text-sm">loading…</p>
        )}
        {albumsQ.error && (
          <p className="text-danger text-sm">
            error: {(albumsQ.error as Error).message}
          </p>
        )}
        {albums.length > 0 && (
          <div className="tile-grid">
            {albums.slice(0, ALBUMS_N).map((a) => (
              <AlbumCard key={a.id} album={a} />
            ))}
          </div>
        )}
      </div>

      <div className="section">
        <div className="section-head">
          <h2>recently active artists</h2>
          <Link to="/artists" className="section-more">
            all artists
          </Link>
        </div>
        {recentArtists.length > 0 && (
          <div className="tile-grid">
            {recentArtists.map((a) => (
              <ArtistCard key={a.id} artist={a} />
            ))}
          </div>
        )}
      </div>

      <div className="section">
        <div className="section-head">
          <h2>recently added tracks</h2>
          <Link to="/tracks" className="section-more">
            all tracks
          </Link>
        </div>
        {tracksQ.isLoading && (
          <p className="text-fg-muted text-sm">loading…</p>
        )}
        {tracksQ.error && (
          <p className="text-danger text-sm">
            error: {(tracksQ.error as Error).message}
          </p>
        )}
        {tracksQ.data && tracksQ.data.length > 0 && (
          <TrackTable
            tracks={tracksQ.data}
            showAlbum
            onPlay={(i) => playSingle(sync, tracksQ.data![i]!)}
          />
        )}
      </div>
    </Layout>
  );
}

// Dedupe artistId from a recent-albums list, preserving order. The
// album row gives us id + name; we look up coverArt + albumCount from
// the canonical /rest/getArtists list. Falls back to the album's own
// coverArt if the canonical record has none — visually fine, since the
// album cover is genuinely "this artist's most recent thing".
function deriveRecentArtists(
  albums: Album[],
  canonical: Artist[],
  limit: number
): Artist[] {
  const lookup = new Map<string, Artist>();
  for (const a of canonical) lookup.set(a.id, a);
  const seen = new Set<string>();
  const out: Artist[] = [];
  for (const a of albums) {
    if (!a.artistId || !a.artist) continue;
    if (seen.has(a.artistId)) continue;
    seen.add(a.artistId);
    const cn = lookup.get(a.artistId);
    out.push({
      id: a.artistId,
      name: cn?.name ?? a.artist,
      ...(cn?.coverArt ?? a.coverArt
        ? { coverArt: cn?.coverArt ?? a.coverArt }
        : {}),
      ...(cn?.albumCount !== undefined ? { albumCount: cn.albumCount } : {}),
    });
    if (out.length >= limit) break;
  }
  return out;
}
