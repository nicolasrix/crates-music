// "Liked" — everything the user has liked: songs, albums, and artists,
// newest-liked first within each section.
//
// The gateway's /v1/library/ratings returns kind + id + verdict only (its
// metadata has no cover art), so we hydrate each liked id client-side via
// the Subsonic getSong / getAlbum / getArtist paths — the same hydration
// pattern the recommend surfaces use. Individual hydration failures (a liked
// entity since removed from the catalog) are tolerated rather than failing
// the whole page.

import { useQuery } from "@tanstack/react-query";
import { getAlbum, getArtist, getSong } from "../api/client";
import { getRatings } from "../api/library";
import { AlbumCard } from "../components/AlbumCard";
import { ArtistCard } from "../components/ArtistCard";
import { Layout } from "../components/Layout";
import { TrackTable } from "../components/TrackTable";
import { usePlayback } from "../sync/usePlayback";
import type { Album, Artist, Track } from "../api/types";

/** Keep only the fulfilled values from a settled batch — a liked entity
 *  since removed from the catalog just drops out. */
function fulfilled<T>(settled: PromiseSettledResult<T>[]): T[] {
  return settled
    .filter((s): s is PromiseFulfilledResult<T> => s.status === "fulfilled")
    .map((s) => s.value);
}

export function LikedSongs() {
  const q = useQuery({
    queryKey: ["library", "liked", "hydrated"],
    queryFn: async () => {
      const rows = await getRatings();
      const liked = (kind: "track" | "album" | "artist") =>
        rows.filter((r) => r.kind === kind && r.rating === "like").map((r) => r.id);

      const [tracks, albums, artists] = await Promise.all([
        Promise.allSettled(liked("track").map((id) => getSong(id))).then(fulfilled),
        Promise.allSettled(liked("album").map((id) => getAlbum(id))).then((s) =>
          fulfilled(s).map((a) => a.album),
        ),
        Promise.allSettled(liked("artist").map((id) => getArtist(id))).then((s) =>
          fulfilled(s).map((a) => a.artist),
        ),
      ]);
      return { tracks, albums, artists };
    },
    staleTime: 30_000,
  });
  const { playList, playAlbum } = usePlayback();
  const tracks: Track[] = q.data?.tracks ?? [];
  const albums: Album[] = q.data?.albums ?? [];
  const artists: Artist[] = q.data?.artists ?? [];
  const empty =
    !q.isLoading && tracks.length === 0 && albums.length === 0 && artists.length === 0;

  return (
    <Layout breadcrumb="liked">
      <div className="section">
        <div className="section-head">
          <h2>liked songs</h2>
          {tracks.length > 0 && <span className="count tabular">{tracks.length}</span>}
        </div>
        <p className="lead">songs you've liked — boosted in recommendations.</p>
        {q.isLoading && <p className="text-fg-muted text-sm">loading…</p>}
        {q.error && (
          <p className="text-danger text-sm">error: {(q.error as Error).message}</p>
        )}
        {empty && (
          <p className="text-fg-muted text-sm">
            nothing liked yet — tap the heart in the player, or on an album or
            artist page.
          </p>
        )}
        {tracks.length > 0 && (
          <TrackTable tracks={tracks} showAlbum onPlay={(i) => playList(tracks, i)} />
        )}
      </div>

      {albums.length > 0 && (
        <div className="section">
          <div className="section-head">
            <h2>liked albums</h2>
            <span className="count tabular">{albums.length}</span>
          </div>
          <div className="tile-grid">
            {albums.map((a) => (
              <AlbumCard
                key={a.id}
                album={a}
                onPlay={() => void playAlbum(a.id)}
              />
            ))}
          </div>
        </div>
      )}

      {artists.length > 0 && (
        <div className="section">
          <div className="section-head">
            <h2>liked artists</h2>
            <span className="count tabular">{artists.length}</span>
          </div>
          <div className="tile-grid">
            {artists.map((a) => (
              <ArtistCard key={a.id} artist={a} />
            ))}
          </div>
        </div>
      )}
    </Layout>
  );
}
