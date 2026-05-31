// "Liked songs" — every track the user has liked, newest-liked first.
//
// The gateway's /v1/library/ratings returns ids + verdict only (its
// TrackMetadata has no cover art), so we hydrate each liked id client-side
// via the Subsonic getSong path — the same hydration pattern the
// recommend surfaces use. Individual hydration failures (a liked track
// since removed from the catalog) are tolerated rather than failing the
// whole page.

import { useQuery } from "@tanstack/react-query";
import { getSong } from "../api/client";
import { getRatings } from "../api/library";
import { Layout } from "../components/Layout";
import { TrackTable } from "../components/TrackTable";
import { usePlayback } from "../sync/usePlayback";
import type { Track } from "../api/types";

export function LikedSongs() {
  const q = useQuery({
    queryKey: ["library", "liked", "hydrated"],
    queryFn: async () => {
      const rows = await getRatings();
      const likedIds = rows
        .filter((r) => r.rating === "like")
        .map((r) => r.track_id);
      const settled = await Promise.allSettled(likedIds.map((id) => getSong(id)));
      return settled
        .filter((s): s is PromiseFulfilledResult<Track> => s.status === "fulfilled")
        .map((s) => s.value);
    },
    staleTime: 30_000,
  });
  const { playList } = usePlayback();
  const tracks: Track[] = q.data ?? [];

  return (
    <Layout breadcrumb="liked songs">
      <div className="section">
        <div className="section-head">
          <h2>liked songs</h2>
          {tracks.length > 0 && (
            <span className="count tabular">{tracks.length}</span>
          )}
        </div>
        <p className="lead">songs you've liked — boosted in recommendations.</p>
        {q.isLoading && <p className="text-fg-muted text-sm">loading…</p>}
        {q.error && (
          <p className="text-danger text-sm">
            error: {(q.error as Error).message}
          </p>
        )}
        {q.data && tracks.length === 0 && !q.isLoading && (
          <p className="text-fg-muted text-sm">
            no liked songs yet — tap the heart in the player to like the
            current track.
          </p>
        )}
        {tracks.length > 0 && (
          <TrackTable
            tracks={tracks}
            showAlbum
            onPlay={(i) => playList(tracks, i)}
          />
        )}
      </div>
    </Layout>
  );
}
