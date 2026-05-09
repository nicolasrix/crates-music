// Album detail. Implements the "central mechanism" from the design handoff:
// extracts a palette from the cover, sets --art-bg/fg/mute/accent on <main>,
// renders a hero + tracklist that read those vars. Chrome (sidebar, topbar,
// player bar) stays neutral by intent.

import { useQuery } from "@tanstack/react-query";
import { Plus, MoreHorizontal, Play, Sparkles } from "lucide-react";
import { useState } from "react";
import { coverArtUrl, getAlbum } from "../api/client";
import { SeedNotEmbeddedError, startStationFromAny } from "../api/recommend";
import { Layout } from "../components/Layout";
import { useCoverPalette } from "../components/ArtworkPalette";
import { TrackTable } from "../components/TrackTable";
import { Link } from "../router";
import { usePlayback } from "../sync/usePlayback";
import { fmtDuration } from "../utils/format";
import type { Track } from "../api/types";

export function Album({ id }: { id: string }) {
  const q = useQuery({
    queryKey: ["album", id],
    queryFn: () => getAlbum(id),
  });
  const cover = coverArtUrl(q.data?.album.coverArt, 600);
  const palette = useCoverPalette(cover);
  const { playSingle, playList } = usePlayback();

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

  return (
    <Layout breadcrumb={`albums · ${album.name}`} palette={palette}>
      <div className="tinted-wash" />
      <div className="hero">
        <div className="cover-lg">
          {cover && <img src={cover} alt={album.name} />}
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
            <button className="icon-btn" aria-label="add to queue" title="add to queue">
              <Plus size={18} strokeWidth={1.5} />
            </button>
            <button className="icon-btn" aria-label="more" title="more">
              <MoreHorizontal size={18} strokeWidth={1.5} />
            </button>
          </div>
          <StationStatus status={stationStatus} />
        </div>
      </div>

      <div className="section">
        <TrackTable
          tracks={tracks}
          showAlbum={false}
          onPlay={(i) => playSingle(tracks[i]!)}
        />
      </div>
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
