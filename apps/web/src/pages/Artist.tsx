// Artist detail. Same shape as Album detail (hero + tinted wash + palette
// extraction) but with a circular cover, an optional biography blurb, and
// the artist's albums grid in place of a tracklist.

import { useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { Play } from "lucide-react";
import { useMemo, useState } from "react";
import {
  coverArtUrl,
  getAlbum,
  getArtist,
  getTopSongs,
  listArtists,
} from "../api/client";
import { fetchSimilarArtists } from "../api/recommend";
import { AlbumCard } from "../components/AlbumCard";
import { ArtistHeroCard } from "../components/ArtistHeroCard";
import { ArtistTable } from "../components/ArtistTable";
import { Cover } from "../components/Cover";
import { EntityRating } from "../components/EntityRating";
import { HeroBackdrop } from "../components/HeroBackdrop";
import { Layout } from "../components/Layout";
import { useCoverPalette } from "../components/ArtworkPalette";
import { usePlayback } from "../sync/usePlayback";
import type { Artist as ArtistType } from "../api/types";

// Album-order fallback for "play artist" when getTopSongs has nothing
// (no play history yet). Bounded so a 50-album discography doesn't fan
// out 50 getAlbum calls from one click.
const PLAY_FALLBACK_ALBUMS = 10;

export function Artist({ id }: { id: string }) {
  const q = useQuery({
    queryKey: ["artist", id],
    queryFn: () => getArtist(id),
  });
  const cover = coverArtUrl(q.data?.artist.coverArt, 600, q.data?.artist.name);
  const palette = useCoverPalette(cover);
  const { playList, playAlbum } = usePlayback();
  const queryClient = useQueryClient();
  const [playPending, setPlayPending] = useState(false);

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

  async function playArtist() {
    setPlayPending(true);
    try {
      // Top songs first (play-count-backed); empty for never-played
      // artists, so fall back to the albums grid's listing order.
      // getTopSongs failures degrade to the fallback too.
      let tracks = await getTopSongs(artist.name).catch(() => []);
      if (tracks.length === 0) {
        const details = await Promise.all(
          albums.slice(0, PLAY_FALLBACK_ALBUMS).map((a) =>
            queryClient
              .fetchQuery({
                queryKey: ["album", a.id],
                queryFn: () => getAlbum(a.id),
                staleTime: 5 * 60_000,
              })
              .catch(() => null),
          ),
        );
        tracks = details.flatMap((d) => d?.tracks ?? []);
      }
      if (tracks.length > 0) playList(tracks, 0);
    } finally {
      setPlayPending(false);
    }
  }

  return (
    <Layout breadcrumb={`artists · ${artist.name}`} palette={palette}>
      <div className="hero">
        <HeroBackdrop url={cover} />
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
            <button
              className="play-disc"
              onClick={() => void playArtist()}
              disabled={playPending || albums.length === 0}
              aria-label="play artist"
              title="play artist (top tracks)"
            >
              <Play size={20} fill="currentColor" strokeWidth={0} />
            </button>
            <EntityRating kind="artist" id={artist.id} />
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
              <AlbumCard
                key={a.id}
                album={a}
                onPlay={() => void playAlbum(a.id)}
              />
            ))}
          </div>
        )}
      </div>

      <SimilarArtistsSection
        seedArtistId={artist.id}
        seedAlbumIds={albums.map((a) => a.id)}
      />
    </Layout>
  );
}

// "similar artists" for an artist page. The CLAP recommender keys off
// *tracks*, so we hydrate the first SAMPLE_ALBUMS albums (cache-shared
// with /albums/:id, so mostly free on warm nav) and feed their track
// ids as seeds. Section is hidden entirely when nothing comes back —
// matches the Album page's "no dead empty block" rule.
const SIMILAR_N = 8;
const SAMPLE_ALBUMS = 3;
const TOP_HERO = 3;

function SimilarArtistsSection({
  seedArtistId,
  seedAlbumIds,
}: {
  seedArtistId: string;
  seedAlbumIds: readonly string[];
}) {
  const sampledIds = seedAlbumIds.slice(0, SAMPLE_ALBUMS);
  const albumDetailQs = useQueries({
    queries: sampledIds.map((id) => ({
      queryKey: ["album", id],
      queryFn: () => getAlbum(id),
      staleTime: 5 * 60_000,
    })),
  });
  const albumsReady =
    albumDetailQs.length > 0 && albumDetailQs.every((q) => !q.isLoading);
  const seedTrackIds = useMemo(
    () => albumDetailQs.flatMap((q) => (q.data?.tracks ?? []).map((t) => t.id)),
    [albumDetailQs],
  );
  const hasSeeds = albumsReady && seedTrackIds.length > 0;

  const artistsQ = useQuery({
    queryKey: ["similar-artists-for-artist", seedArtistId, sampledIds.join(",")],
    queryFn: () =>
      fetchSimilarArtists({
        seedTrackIds,
        excludeArtistIds: [seedArtistId],
        n: SIMILAR_N,
      }),
    enabled: hasSeeds,
    staleTime: 5 * 60_000,
  });

  // Hydrate artist ids via the cached registry — same pattern as Album's
  // SimilarSection and /search.
  const knownArtistsQ = useQuery({
    queryKey: ["artists"],
    queryFn: listArtists,
    enabled: hasSeeds,
    staleTime: 5 * 60_000,
  });
  const artistById = useMemo(() => {
    const map = new Map<string, ArtistType>();
    for (const a of knownArtistsQ.data ?? []) map.set(a.id, a);
    return map;
  }, [knownArtistsQ.data]);
  const hydratedArtists = useMemo<ArtistType[]>(
    () =>
      (artistsQ.data?.results ?? [])
        .map((r) => artistById.get(r.artist_id))
        .filter((a): a is ArtistType => a !== undefined),
    [artistsQ.data, artistById],
  );

  if (!hasSeeds || artistsQ.isLoading) return null;
  if (hydratedArtists.length === 0) return null;

  return (
    <div className="section">
      <div className="section-head">
        <h2>similar artists</h2>
      </div>
      <div className="hero-strip">
        {hydratedArtists.slice(0, TOP_HERO).map((a) => (
          <ArtistHeroCard key={a.id} artist={a} />
        ))}
      </div>
      {hydratedArtists.length > TOP_HERO && (
        <ArtistTable artists={hydratedArtists.slice(TOP_HERO)} />
      )}
    </div>
  );
}
