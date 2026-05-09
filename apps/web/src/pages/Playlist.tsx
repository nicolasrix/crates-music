// Playlist detail. Same hero+tracklist as Album detail; the cover is a 2×2
// quilt assembled from the first four contained albums' covers (the README
// pins this layout).

import { useQuery } from "@tanstack/react-query";
import { Play } from "lucide-react";
import { coverArtUrl, getPlaylist } from "../api/client";
import { Layout } from "../components/Layout";
import { useCoverPalette } from "../components/ArtworkPalette";
import { TrackTable } from "../components/TrackTable";
import { useSync } from "../sync/SyncContext";
import { playList, playSingle } from "../sync/playbackHelpers";
import { fmtDuration } from "../utils/format";
import type { Track } from "../api/types";

export function Playlist({ id }: { id: string }) {
  const q = useQuery({
    queryKey: ["playlist", id],
    queryFn: () => getPlaylist(id),
  });
  // Drive palette extraction from the first contained track's cover —
  // the playlist itself often lacks dedicated artwork.
  const seedCover =
    q.data?.tracks.find((t) => t.coverArt)?.coverArt ?? q.data?.playlist.coverArt;
  const palette = useCoverPalette(coverArtUrl(seedCover, 600));
  const sync = useSync();

  if (q.isLoading) {
    return (
      <Layout palette={null}>
        <div className="section">
          <p className="text-fg-muted text-sm">loading…</p>
        </div>
      </Layout>
    );
  }
  if (q.error || !q.data) {
    return (
      <Layout palette={null}>
        <div className="section">
          <p className="text-danger text-sm">
            error: {(q.error as Error | undefined)?.message ?? "not found"}
          </p>
        </div>
      </Layout>
    );
  }

  const { playlist, tracks } = q.data;
  const totalSeconds = tracks.reduce((sum, t) => sum + (t.duration ?? 0), 0);
  const quiltCovers = pickQuiltCovers(tracks);

  return (
    <Layout breadcrumb={`playlists · ${playlist.name}`} palette={palette}>
      <div className="tinted-wash" />
      <div className="hero">
        <div className="cover-lg">
          <Quilt urls={quiltCovers} fallback={playlist.name} />
        </div>
        <div className="meta-stack">
          <div className="kind">playlist</div>
          <h1>{playlist.name}</h1>
          <div className="sub">
            {tracks.length > 0 && (
              <span>
                {tracks.length} track{tracks.length === 1 ? "" : "s"}
              </span>
            )}
            {totalSeconds > 0 && <span aria-hidden>·</span>}
            {totalSeconds > 0 && <span>{fmtDuration(totalSeconds)}</span>}
          </div>
          <div className="actions">
            <button
              className="play-disc"
              onClick={() => playList(sync, tracks, 0)}
              aria-label="play playlist"
            >
              <Play size={20} fill="currentColor" strokeWidth={0} />
            </button>
          </div>
        </div>
      </div>
      <div className="section">
        <TrackTable
          tracks={tracks}
          showAlbum
          onPlay={(i) => playSingle(sync, tracks[i]!)}
        />
      </div>
    </Layout>
  );
}

function pickQuiltCovers(tracks: Track[]): string[] {
  // Pick up to four distinct album covers in track order — keeps the quilt
  // visually varied even when many consecutive tracks share an album.
  const seen = new Set<string>();
  const out: string[] = [];
  for (const t of tracks) {
    if (!t.coverArt) continue;
    const url = coverArtUrl(t.coverArt, 300);
    if (!url || seen.has(url)) continue;
    seen.add(url);
    out.push(url);
    if (out.length === 4) break;
  }
  return out;
}

function Quilt({ urls, fallback }: { urls: string[]; fallback: string }) {
  if (urls.length === 0) {
    return (
      <div className="w-full h-full flex items-center justify-center text-fg-faint text-xs">
        {fallback}
      </div>
    );
  }
  // Repeat what we have until we have four cells — preserves the 2×2 layout
  // even when the playlist has fewer than four distinct album covers.
  const cells = [...urls];
  while (cells.length < 4) cells.push(cells[cells.length % urls.length]!);
  return (
    <div className="cover-quilt">
      {cells.slice(0, 4).map((u, i) => (
        <div key={i} style={{ backgroundImage: `url(${u})` }} aria-hidden />
      ))}
    </div>
  );
}
