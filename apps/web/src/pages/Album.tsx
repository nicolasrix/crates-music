// Album detail. Implements the "central mechanism" from the design handoff:
// extracts a palette from the cover, sets --art-bg/fg/mute/accent on <main>,
// renders a hero + tracklist that read those vars. Chrome (sidebar, topbar,
// player bar) stays neutral by intent.

import { useQueries, useQuery } from "@tanstack/react-query";
import { Plus, Play, Sparkles } from "lucide-react";
import { useMemo, useState } from "react";
import { coverArtUrl, getAlbum, listArtists } from "../api/client";
import {
  SeedNotEmbeddedError,
  fetchSimilarAlbums,
  fetchSimilarArtists,
  startStationFromAny,
} from "../api/recommend";
import { AlbumHeroCard } from "../components/AlbumHeroCard";
import { AlbumTable } from "../components/AlbumTable";
import { ArtistHeroCard } from "../components/ArtistHeroCard";
import { EntityRating } from "../components/EntityRating";
import { ArtistTable } from "../components/ArtistTable";
import { Cover } from "../components/Cover";
import { DownloadAllButton } from "../components/DownloadAllButton";
import { HeroBackdrop } from "../components/HeroBackdrop";
import { Layout } from "../components/Layout";
import { useCoverPalette } from "../components/ArtworkPalette";
import { TrackTable } from "../components/TrackTable";
import { Link } from "../router";
import { useSync } from "../sync/SyncContext";
import { usePlayback } from "../sync/usePlayback";
import { useToast } from "../toast/ToastContext";
import { fmtDuration, fmtPlays, fmtRelativePast } from "../utils/format";
import type { Album as AlbumType, Artist, Track } from "../api/types";

export function Album({ id }: { id: string }) {
  const q = useQuery({
    queryKey: ["album", id],
    queryFn: () => getAlbum(id),
  });
  const cover = coverArtUrl(q.data?.album.coverArt, 600, q.data?.album.name);
  const palette = useCoverPalette(cover);
  const { playList } = usePlayback();
  const sync = useSync();
  const toast = useToast();

  // Station state — surface "loading" / "not indexed" inline near the hero
  // actions row rather than as a toast, so the failure mode is co-located
  // with the trigger.
  const [stationStatus, setStationStatus] = useState<
    | { kind: "idle" }
    | { kind: "loading" }
    | { kind: "empty" }
    | { kind: "not_indexed" }
    | { kind: "error"; message: string }
  >({ kind: "idle" });

  async function startStationForAlbum(albumTracks: Track[]) {
    if (albumTracks.length === 0) return;
    setStationStatus({ kind: "loading" });
    try {
      // Walk the album's tracks in order; the first one that's already in
      // the ANN seeds the station. Only call the album "not indexed" if
      // every track 404s — a single unindexed track is normal at our
      // current ingest coverage.
      const { tracks } = await startStationFromAny(
        albumTracks.map((t) => t.id),
        20
      );
      if (tracks.length === 0) {
        setStationStatus({ kind: "empty" });
        return;
      }
      playList(tracks, 0);
      setStationStatus({ kind: "idle" });
    } catch (e) {
      if (e instanceof SeedNotEmbeddedError) {
        setStationStatus({ kind: "not_indexed" });
      } else {
        setStationStatus({ kind: "error", message: (e as Error).message });
      }
    }
  }

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

  const { album, tracks } = q.data;
  const totalSeconds = tracks.reduce((sum, t) => sum + (t.duration ?? 0), 0);
  // Per A2's "derive on read" decision (see project_play_counts_decisions.md):
  // when Navidrome doesn't surface album.playCount itself, fall back to
  // summing track-level counts. Either path produces the same number for
  // the user; this keeps the hero meaningful even on older Subsonic builds.
  const albumPlays = album.playCount ?? tracks.reduce((sum, t) => sum + (t.playCount ?? 0), 0);
  const playsLabel = fmtPlays(albumPlays);
  const lastPlayedLabel = fmtRelativePast(album.played);

  return (
    <Layout breadcrumb={`albums · ${album.name}`} palette={palette}>
      <div className="hero">
        <HeroBackdrop url={cover} />
        <div className="cover-lg">
          <Cover
            coverArt={album.coverArt}
            seed={album.name}
            size={600}
            alt={album.name}
            loading="eager"
          />
        </div>
        <div className="meta-stack">
          <div className="kind">album</div>
          <h1>{album.name}</h1>
          <div className="sub">
            {album.artistId && album.artist ? (
              <Link to={`/artists/${album.artistId}`} className="sub-link">
                {album.artist}
              </Link>
            ) : (
              <span>{album.artist ?? "—"}</span>
            )}
            {album.year && <span aria-hidden>·</span>}
            {album.year && <span>{album.year}</span>}
            {tracks.length > 0 && <span aria-hidden>·</span>}
            {tracks.length > 0 && (
              <span>
                {tracks.length} track{tracks.length === 1 ? "" : "s"}
              </span>
            )}
            {totalSeconds > 0 && <span aria-hidden>·</span>}
            {totalSeconds > 0 && <span>{fmtDuration(totalSeconds)}</span>}
            {playsLabel && <span aria-hidden>·</span>}
            {playsLabel && <span>{playsLabel}</span>}
            {lastPlayedLabel && <span aria-hidden>·</span>}
            {lastPlayedLabel && <span>last played {lastPlayedLabel}</span>}
          </div>
          <div className="actions">
            <button
              className="play-disc"
              onClick={() => playList(tracks, 0)}
              aria-label="play album"
              title="play album"
            >
              <Play size={20} fill="currentColor" strokeWidth={0} />
            </button>
            <button
              className="icon-btn"
              onClick={() => startStationForAlbum(tracks)}
              disabled={tracks.length === 0 || stationStatus.kind === "loading"}
              aria-label="start station"
              title="start station — play tracks similar to this album"
            >
              <Sparkles size={18} strokeWidth={1.5} />
            </button>
            <button
              className="icon-btn"
              onClick={() => {
                for (const t of tracks) sync.pushTrack(t);
                toast(
                  `added ${tracks.length} track${tracks.length === 1 ? "" : "s"} to queue`,
                  { variant: "success" },
                );
              }}
              disabled={tracks.length === 0}
              aria-label="add album to queue"
              title="add album to queue"
            >
              <Plus size={18} strokeWidth={1.5} />
            </button>
            <DownloadAllButton tracks={tracks} label="download album for offline" />
            <EntityRating kind="album" id={album.id} />
          </div>
          <StationStatus status={stationStatus} />
        </div>
      </div>

      <div className="section">
        <TrackTable
          tracks={tracks}
          showAlbum={false}
          onPlay={(i) => playList(tracks, i)}
        />
      </div>

      <SimilarSection
        seedTrackIds={tracks.map((t) => t.id)}
        seedAlbumId={album.id}
        seedArtistId={album.artistId ?? null}
      />
    </Layout>
  );
}

function StationStatus({
  status,
}: {
  status:
    | { kind: "idle" }
    | { kind: "loading" }
    | { kind: "empty" }
    | { kind: "not_indexed" }
    | { kind: "error"; message: string };
}) {
  if (status.kind === "idle") return null;
  // Engineer-direct microcopy per design voice — no "Oops!", no exclamation
  // marks. Each line tells the user what happened and what (if anything) to
  // do next.
  const label =
    status.kind === "loading"
      ? "starting station…"
      : status.kind === "empty"
        ? "no similar tracks found yet."
        : status.kind === "not_indexed"
          ? "this track isn't embedded yet — try another album."
          : `error: ${status.message}`;
  const tone =
    status.kind === "error" || status.kind === "not_indexed"
      ? "text-danger"
      : "text-art-mute";
  return (
    <p className={`text-xs mt-2 ${tone}`} style={{ minHeight: "1.2em" }}>
      {label}
    </p>
  );
}

// "you might like" — runs CLAP-similarity on the album's tracks and
// aggregates by album_id / artist_id server-side. Renders nothing while
// loading, and nothing when both buckets come back empty (e.g. the
// recommender hasn't ingested this album yet). Side-by-side via the
// shared .search-grid CSS used on /search.
const SIMILAR_N = 8;
const TOP_HERO = 3;

function SimilarSection({
  seedTrackIds,
  seedAlbumId,
  seedArtistId,
}: {
  seedTrackIds: readonly string[];
  seedAlbumId: string;
  seedArtistId: string | null;
}) {
  const hasSeeds = seedTrackIds.length > 0;
  const { playAlbum } = usePlayback();

  const albumsQ = useQuery({
    queryKey: ["similar-albums", seedAlbumId],
    queryFn: () =>
      fetchSimilarAlbums({
        seedTrackIds,
        excludeAlbumIds: [seedAlbumId],
        n: SIMILAR_N,
      }),
    enabled: hasSeeds,
    staleTime: 5 * 60_000,
  });

  const artistsQ = useQuery({
    queryKey: ["similar-artists", seedAlbumId],
    queryFn: () =>
      fetchSimilarArtists({
        seedTrackIds,
        // Exclude the current artist when known; otherwise let the
        // recommender's own artist-cap pull in close artists.
        ...(seedArtistId ? { excludeArtistIds: [seedArtistId] } : {}),
        n: SIMILAR_N,
      }),
    enabled: hasSeeds,
    staleTime: 5 * 60_000,
  });

  // Hydrate album ids → full Album shapes. One getAlbum call per
  // recommendation, parallel via useQueries. Cache key is shared with
  // the /albums/:id page so navigating into a recommendation is a hit
  // on the L2 cache.
  const albumIds = albumsQ.data?.results.map((r) => r.album_id) ?? [];
  const albumDetailQs = useQueries({
    queries: albumIds.map((id) => ({
      queryKey: ["album", id],
      queryFn: () => getAlbum(id),
      staleTime: 5 * 60_000,
    })),
  });
  const hydratedAlbums = useMemo<AlbumType[]>(
    () =>
      albumDetailQs
        .map((q) => q.data?.album)
        .filter((a): a is AlbumType => a !== undefined),
    [albumDetailQs],
  );

  // Hydrate artists by mapping ids against the cached registry. listArtists
  // is shared with /home and /search, so this is a no-op fetch most of the
  // time.
  const knownArtistsQ = useQuery({
    queryKey: ["artists"],
    queryFn: listArtists,
    enabled: hasSeeds,
    staleTime: 5 * 60_000,
  });
  const artistById = useMemo(() => {
    const map = new Map<string, Artist>();
    for (const a of knownArtistsQ.data ?? []) map.set(a.id, a);
    return map;
  }, [knownArtistsQ.data]);
  const hydratedArtists = useMemo<Artist[]>(
    () =>
      (artistsQ.data?.results ?? [])
        .map((r) => artistById.get(r.artist_id))
        .filter((a): a is Artist => a !== undefined),
    [artistsQ.data, artistById],
  );

  // While both queries are still loading we render nothing — flashing an
  // empty stub before data arrives would just push the player bar around.
  const stillLoading = albumsQ.isLoading || artistsQ.isLoading;
  if (stillLoading) return null;

  const albumsSection =
    hydratedAlbums.length > 0 ? (
      <SimilarBucket
        heading="similar albums"
        hero={hydratedAlbums.slice(0, TOP_HERO).map((a) => (
          <AlbumHeroCard
            key={a.id}
            album={a}
            onPlay={() => void playAlbum(a.id)}
          />
        ))}
        rest={
          hydratedAlbums.length > TOP_HERO ? (
            <AlbumTable
              albums={hydratedAlbums.slice(TOP_HERO)}
              onPlayAlbum={(a) => void playAlbum(a.id)}
            />
          ) : null
        }
      />
    ) : null;

  const artistsSection =
    hydratedArtists.length > 0 ? (
      <SimilarBucket
        heading="similar artists"
        hero={hydratedArtists
          .slice(0, TOP_HERO)
          .map((a) => <ArtistHeroCard key={a.id} artist={a} />)}
        rest={
          hydratedArtists.length > TOP_HERO ? (
            <ArtistTable artists={hydratedArtists.slice(TOP_HERO)} />
          ) : null
        }
      />
    ) : null;

  // Hide the whole section when both buckets are empty — keeps the page
  // from ending in a dead "you might like (nothing)" block.
  if (!albumsSection && !artistsSection) return null;

  // Two-column wrap only when both buckets have content. Otherwise a
  // single bucket flows full-width like a normal section so the page
  // doesn't end up with a half-empty grid row.
  if (albumsSection && artistsSection) {
    return (
      <div className="search-grid">
        {artistsSection}
        {albumsSection}
      </div>
    );
  }
  return (
    <>
      {artistsSection}
      {albumsSection}
    </>
  );
}

function SimilarBucket({
  heading,
  hero,
  rest,
}: {
  heading: string;
  hero: readonly React.ReactNode[];
  rest: React.ReactNode;
}) {
  return (
    <div className="section">
      <div className="section-head">
        <h2>{heading}</h2>
      </div>
      <div className="hero-strip">{hero}</div>
      {rest}
    </div>
  );
}
