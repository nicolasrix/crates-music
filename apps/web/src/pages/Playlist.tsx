// Playlist detail. Same hero+tracklist as Album detail; the cover is a 2×2
// quilt assembled from the first four contained albums' covers (the README
// pins this layout).

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Pencil, Play, Plus, Sparkles, Trash2 } from "lucide-react";
import { useState } from "react";
import { coverArtUrl } from "../api/client";
import {
  addTrackToPlaylist,
  deletePlaylist,
  getPlaylist,
  renamePlaylist,
} from "../api/playlists";
import { suggestForPlaylist } from "../api/recommend";
import { Cover } from "../components/Cover";
import { DownloadAllButton } from "../components/DownloadAllButton";
import { Layout } from "../components/Layout";
import { useCoverPalette } from "../components/ArtworkPalette";
import { TrackTable } from "../components/TrackTable";
import { navigate } from "../router";
import { usePlayback } from "../sync/usePlayback";
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
  const palette = useCoverPalette(
    coverArtUrl(seedCover, 600, q.data?.playlist.name),
  );
  const { playSingle, playList } = usePlayback();
  const queryClient = useQueryClient();

  // Lightweight dialogs — same prompt-based UX as new-playlist creation
  // in Sidebar. Once a proper modal primitive lands, swap these two and
  // Sidebar's create flow over together.
  async function handleRename() {
    if (!q.data) return;
    const raw = window.prompt("rename playlist", q.data.playlist.name);
    if (raw === null) return;
    const name = raw.trim();
    if (name.length === 0 || name === q.data.playlist.name) return;
    try {
      await renamePlaylist(id, name);
      // Refresh both surfaces: this page's hero/breadcrumb and the
      // sidebar list. Same key as TrackRowMenu and Sidebar use.
      await queryClient.invalidateQueries({ queryKey: ["playlist", id] });
      await queryClient.invalidateQueries({ queryKey: ["playlists"] });
    } catch (e) {
      window.alert(`couldn't rename playlist: ${(e as Error).message}`);
    }
  }

  async function handleDelete() {
    if (!q.data) return;
    const ok = window.confirm(
      `delete playlist "${q.data.playlist.name}"? this can't be undone.`
    );
    if (!ok) return;
    try {
      await deletePlaylist(id);
      await queryClient.invalidateQueries({ queryKey: ["playlists"] });
      // The current route is now stale — navigate home rather than leave
      // the user staring at a deleted playlist's tracks.
      navigate("/");
    } catch (e) {
      window.alert(`couldn't delete playlist: ${(e as Error).message}`);
    }
  }

  // Suggestion state. Local, ephemeral — refetched on every "suggest"
  // click. Tracks added to the playlist are spliced out optimistically
  // so the user can keep adding without the row they just acted on
  // hanging around.
  const [suggestState, setSuggestState] = useState<
    | { kind: "idle" }
    | { kind: "loading" }
    | { kind: "ready"; tracks: Track[] }
    | { kind: "empty" }
    | { kind: "all_unindexed" }
    | { kind: "error"; message: string }
  >({ kind: "idle" });
  const [addingId, setAddingId] = useState<string | null>(null);

  async function handleSuggest() {
    if (!q.data) return;
    const trackIds = q.data.tracks.map((t) => t.id);
    if (trackIds.length === 0) return;
    setSuggestState({ kind: "loading" });
    try {
      const result = await suggestForPlaylist(trackIds);
      if (result.allSeedsUnindexed) {
        setSuggestState({ kind: "all_unindexed" });
        return;
      }
      if (result.tracks.length === 0) {
        setSuggestState({ kind: "empty" });
        return;
      }
      setSuggestState({ kind: "ready", tracks: result.tracks });
    } catch (e) {
      setSuggestState({ kind: "error", message: (e as Error).message });
    }
  }

  async function handleAddSuggestion(track: Track) {
    if (addingId) return;
    setAddingId(track.id);
    try {
      await addTrackToPlaylist(id, track.id);
      // Refresh the playlist body so the tracklist + count + duration
      // reflect the new track without a manual reload.
      await queryClient.invalidateQueries({ queryKey: ["playlist", id] });
      // Remove from the suggestions panel so the user doesn't add it
      // twice. Cheaper than refetching — the remaining suggestions
      // were ranked against the prior playlist state, but they're still
      // valid; one more track in the playlist won't change the rank
      // ordering meaningfully for a casual "find more" workflow.
      setSuggestState((s) =>
        s.kind === "ready"
          ? { kind: "ready", tracks: s.tracks.filter((t) => t.id !== track.id) }
          : s
      );
    } catch (e) {
      window.alert(`couldn't add track: ${(e as Error).message}`);
    } finally {
      setAddingId(null);
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
              onClick={() => playList(tracks, 0)}
              aria-label="play playlist"
            >
              <Play size={20} fill="currentColor" strokeWidth={0} />
            </button>
            <button
              className="icon-btn"
              onClick={handleRename}
              aria-label="rename playlist"
              title="rename playlist"
            >
              <Pencil size={18} strokeWidth={1.5} />
            </button>
            <button
              className="icon-btn"
              onClick={handleSuggest}
              disabled={
                tracks.length === 0 || suggestState.kind === "loading"
              }
              aria-label="suggest more tracks"
              title="suggest more tracks like these"
            >
              <Sparkles size={18} strokeWidth={1.5} />
            </button>
            <DownloadAllButton tracks={tracks} label="download playlist for offline" />
            <button
              className="icon-btn"
              onClick={handleDelete}
              aria-label="delete playlist"
              title="delete playlist"
            >
              <Trash2 size={18} strokeWidth={1.5} />
            </button>
          </div>
        </div>
      </div>
      <div className="section">
        <TrackTable
          tracks={tracks}
          showAlbum
          onPlay={(i) => playSingle(tracks[i]!)}
        />
      </div>
      <SuggestionsPanel
        state={suggestState}
        addingId={addingId}
        onAdd={handleAddSuggestion}
        onPlay={(t) => playSingle(t)}
        onRetry={handleSuggest}
      />
    </Layout>
  );
}

function SuggestionsPanel({
  state,
  addingId,
  onAdd,
  onPlay,
  onRetry,
}: {
  state:
    | { kind: "idle" }
    | { kind: "loading" }
    | { kind: "ready"; tracks: Track[] }
    | { kind: "empty" }
    | { kind: "all_unindexed" }
    | { kind: "error"; message: string };
  addingId: string | null;
  onAdd: (t: Track) => void;
  onPlay: (t: Track) => void;
  onRetry: () => void;
}) {
  if (state.kind === "idle") return null;
  return (
    <div className="section">
      <div className="section-head">
        <h2>suggestions</h2>
        {state.kind === "ready" && (
          <button
            type="button"
            className="section-more"
            onClick={onRetry}
          >
            refresh
          </button>
        )}
      </div>
      {state.kind === "loading" && (
        <p className="text-fg-muted text-sm">finding similar tracks…</p>
      )}
      {state.kind === "empty" && (
        <p className="text-fg-muted text-sm">
          no further suggestions — your playlist may already cover the
          neighbourhood.
        </p>
      )}
      {state.kind === "all_unindexed" && (
        <p className="text-fg-muted text-sm">
          none of the tracks in this playlist are indexed by the
          recommender yet — try again once ingestion has caught up.
        </p>
      )}
      {state.kind === "error" && (
        <p className="text-danger text-sm">error: {state.message}</p>
      )}
      {state.kind === "ready" && state.tracks.length === 0 && (
        <p className="text-fg-muted text-sm">
          all suggestions added — refresh to look further afield.
        </p>
      )}
      {state.kind === "ready" && state.tracks.length > 0 && (
        <ul className="suggestion-list">
          {state.tracks.map((t) => (
            <SuggestionRow
              key={t.id}
              track={t}
              busy={addingId === t.id}
              onAdd={() => onAdd(t)}
              onPlay={() => onPlay(t)}
            />
          ))}
        </ul>
      )}
    </div>
  );
}

function SuggestionRow({
  track,
  busy,
  onAdd,
  onPlay,
}: {
  track: Track;
  busy: boolean;
  onAdd: () => void;
  onPlay: () => void;
}) {
  return (
    <li className="suggestion-row">
      <button
        type="button"
        className="suggestion-cover"
        onClick={onPlay}
        aria-label={`play ${track.title}`}
      >
        <Cover
          coverArt={track.coverArt}
          seed={track.album ?? track.title}
          size={80}
          alt=""
        />
        <span className="suggestion-cover-play" aria-hidden>
          <Play size={16} fill="currentColor" strokeWidth={0} />
        </span>
      </button>
      <div className="suggestion-meta">
        <div className="suggestion-title">{track.title}</div>
        <div className="suggestion-sub">
          {track.artist ?? "—"}
          {track.album && (
            <>
              <span aria-hidden> · </span>
              {track.album}
            </>
          )}
        </div>
      </div>
      <button
        type="button"
        className="suggestion-add"
        onClick={onAdd}
        disabled={busy}
        aria-label={`add ${track.title} to playlist`}
      >
        <Plus size={14} strokeWidth={2} />
        <span>{busy ? "adding…" : "add"}</span>
      </button>
    </li>
  );
}

function pickQuiltCovers(tracks: Track[]): string[] {
  // Pick up to four distinct album covers in track order — keeps the quilt
  // visually varied even when many consecutive tracks share an album.
  const seen = new Set<string>();
  const out: string[] = [];
  for (const t of tracks) {
    if (!t.coverArt) continue;
    const url = coverArtUrl(t.coverArt, 300, t.album ?? t.title);
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
