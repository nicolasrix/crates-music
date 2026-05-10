// Artist detail. Same shape as Album detail (hero + tinted wash + palette
// extraction) but with a circular cover, an optional biography blurb, and
// the artist's albums grid in place of a tracklist.

import { useQuery } from "@tanstack/react-query";
import { Play } from "lucide-react";
import { coverArtUrl, getArtist } from "../api/client";
import { AlbumCard } from "../components/AlbumCard";
import { Cover } from "../components/Cover";
import { Layout } from "../components/Layout";
import { useCoverPalette } from "../components/ArtworkPalette";

export function Artist({ id }: { id: string }) {
  const q = useQuery({
    queryKey: ["artist", id],
    queryFn: () => getArtist(id),
  });
  const cover = coverArtUrl(q.data?.artist.coverArt, 600, q.data?.artist.name);
  const palette = useCoverPalette(cover);

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

  const { artist, albums, biography } = q.data;

  return (
    <Layout breadcrumb={`artists · ${artist.name}`} palette={palette}>
      <div className="tinted-wash" />
      <div className="hero">
        <div className="cover-lg is-circle">
          <Cover
            coverArt={artist.coverArt}
            seed={artist.name}
            size={600}
            alt={artist.name}
            loading="eager"
          />
        </div>
        <div className="meta-stack">
          <div className="kind">artist</div>
          <h1>{artist.name}</h1>
          <div className="sub">
            {artist.albumCount != null && (
              <span>
                {artist.albumCount} album{artist.albumCount === 1 ? "" : "s"}
              </span>
            )}
          </div>
          {biography && (
            <p className="text-art-mute text-sm max-w-2xl mt-2">{biography}</p>
          )}
          <div className="actions">
            <button className="play-disc" aria-label="play artist" title="play artist (top tracks)">
              <Play size={20} fill="currentColor" strokeWidth={0} />
            </button>
          </div>
        </div>
      </div>

      <div className="section">
        <div className="section-head">
          <h2>albums</h2>
          {albums.length > 0 && (
            <span className="count tabular">{albums.length}</span>
          )}
        </div>
        {albums.length === 0 ? (
          <p className="text-fg-muted text-sm">no albums.</p>
        ) : (
          <div className="tile-grid">
            {albums.map((a) => (
              <AlbumCard key={a.id} album={a} />
            ))}
          </div>
        )}
      </div>
    </Layout>
  );
}
