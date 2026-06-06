// Three-dot context menu for a track row. Renders a `MoreHorizontal`
// trigger; opening it positions a popover via fixed coords (computed from
// the trigger's getBoundingClientRect) so we sidestep any overflow:hidden
// on parent table cells. Click-outside / Escape close the menu.
//
// Submenu pattern: clicking "add to playlist" replaces the menu body with
// the playlist picker rather than fanning out a second floating panel.
// Single-pane is plenty for the current actions and avoids edge cases
// with nested fixed-position elements + outside-click detection.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ChevronRight,
  Disc3,
  Download,
  ListPlus,
  MoreHorizontal,
  Plus,
  Trash2,
  User,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  addTrackToPlaylist,
  createPlaylist,
  listPlaylists,
} from "../api/client";
import { useAudioCache } from "../cache/AudioCacheContext";
import { formatBytes } from "../cache/format";
import { navigate } from "../router";
import { useSync } from "../sync/SyncContext";
import type { Track } from "../api/types";

/**
 * @param showQueueActions  Whether to render "play next" / "add to queue".
 *   Default `true`. Set to `false` for rows whose track is *already* in
 *   the queue (the Queue page) — those actions don't make sense there.
 */
export function TrackRowMenu({
  track,
  showQueueActions = true,
}: {
  track: Track;
  showQueueActions?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [view, setView] = useState<"root" | "playlists">("root");
  // Coords are either top-anchored (menu grows downward from a top edge) or
  // bottom-anchored (menu grows upward from a bottom edge). Bottom-anchoring
  // is used when the menu has to flip above the trigger — it lets us avoid
  // predicting the menu's height, which varies with the number of entries.
  type Coords =
    | { left: number; top: number }
    | { left: number; bottom: number };
  const [coords, setCoords] = useState<Coords | null>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);

  function openAt() {
    const el = triggerRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    // Anchor right edge of menu to the trigger; render below by default.
    // If we'd run off the bottom, flip above and anchor by `bottom` so we
    // don't need to know the menu's actual height (it varies with the
    // number of entries — 3 items in the player vs ~6 in a track table —
    // and a single `MENU_H_GUESS` over-predicts in the small case,
    // leaving a visible gap above the trigger).
    const MENU_W = 220;
    const MENU_H_GUESS = 220;
    const left = Math.min(
      window.innerWidth - MENU_W - 8,
      Math.max(8, rect.right - MENU_W)
    );
    const wouldOverflowBelow =
      rect.bottom + 4 + MENU_H_GUESS > window.innerHeight;
    const next: Coords = wouldOverflowBelow
      ? { left, bottom: Math.max(8, window.innerHeight - rect.top + 4) }
      : { left, top: rect.bottom + 4 };
    setCoords(next);
    setView("root");
    setOpen(true);
  }

  return (
    <>
      <button
        ref={triggerRef}
        className="row-menu-trigger"
        onClick={(e) => {
          e.stopPropagation();
          if (open) setOpen(false);
          else openAt();
        }}
        aria-label="track options"
        aria-haspopup="menu"
        aria-expanded={open}
      >
        <MoreHorizontal size={16} strokeWidth={1.5} />
      </button>
      {open && coords && (
        <Popover
          coords={coords}
          onClose={() => setOpen(false)}
          view={view}
          setView={setView}
          track={track}
          showQueueActions={showQueueActions}
        />
      )}
    </>
  );
}

type PopoverCoords =
  | { left: number; top: number }
  | { left: number; bottom: number };

function Popover({
  coords,
  onClose,
  view,
  setView,
  track,
  showQueueActions,
}: {
  coords: PopoverCoords;
  onClose: () => void;
  view: "root" | "playlists";
  setView: (v: "root" | "playlists") => void;
  track: Track;
  showQueueActions: boolean;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const sync = useSync();
  const queryClient = useQueryClient();
  const cache = useAudioCache();
  const downloaded = cache.isDownloaded(track.id);
  const [busy, setBusy] = useState(false);

  async function toggleDownload() {
    setBusy(true);
    try {
      if (downloaded) {
        await cache.removeDownload(track.id);
      } else {
        const outcome = await cache.download(track.id);
        if (outcome.kind === "would-exceed-budget") {
          window.alert(
            `Not enough offline space — short by ${formatBytes(outcome.overBy)}. ` +
              `Raise the download budget in Settings.`,
          );
        }
      }
    } catch (e) {
      window.alert(`couldn't update download: ${(e as Error).message}`);
    } finally {
      setBusy(false);
      onClose();
    }
  }

  // Click-outside + Escape.
  useEffect(() => {
    function onDown(e: MouseEvent) {
      if (!ref.current) return;
      if (!ref.current.contains(e.target as Node)) onClose();
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  function playNext() {
    // The queue is gateway-managed. Both push and reorder go over the WS
    // and the gateway is the single linearizer, so within one connection
    // the push lands first and reorder finds the new item by id.
    //
    // Edge cases:
    //  1) Empty queue / no cursor → "play next" implicitly means "play
    //     this." We push, set the cursor, and start playback so the
    //     button does the obvious thing instead of silently enqueuing.
    //  2) Cursor exists → push to end, reorder to cursor+1. We compute
    //     +1 against *local* state; in theory auto-advance could shift
    //     the cursor between our push and reorder, putting the track one
    //     slot too far. Single-user reality means this is unobservable,
    //     but worth noting if we ever go multi-client.
    const itemId = sync.pushTrack(track);
    const { now_playing_index: i, queue } = sync.state.playback;
    if (i === null) {
      // The pushed item lands at the current end of the gateway queue.
      // Locally the push hasn't applied yet, so the new index *will be*
      // queue.items.length (after the push lands).
      const newIdx = queue.items.length;
      sync.submit({ type: "set_now_playing", index: newIdx });
      sync.submit({ type: "set_playing", is_playing: true });
    } else {
      const targetIndex = Math.min(i + 1, queue.items.length);
      sync.submit({ type: "reorder", item_id: itemId, new_index: targetIndex });
    }
    onClose();
  }

  function addToQueue() {
    sync.pushTrack(track);
    onClose();
  }

  async function newPlaylistAndAdd() {
    const raw = window.prompt("playlist name");
    if (raw === null) return;
    const name = raw.trim();
    if (name.length === 0) return;
    try {
      const created = await createPlaylist(name);
      if (created.id) {
        await addTrackToPlaylist(created.id, track.id);
      }
      await queryClient.invalidateQueries({ queryKey: ["playlists"] });
    } catch (e) {
      window.alert(`couldn't create playlist: ${(e as Error).message}`);
    } finally {
      onClose();
    }
  }

  async function addToExisting(playlistId: string) {
    try {
      await addTrackToPlaylist(playlistId, track.id);
      await queryClient.invalidateQueries({ queryKey: ["playlists"] });
      await queryClient.invalidateQueries({ queryKey: ["playlist", playlistId] });
    } catch (e) {
      window.alert(`couldn't add to playlist: ${(e as Error).message}`);
    } finally {
      onClose();
    }
  }

  // Portal into <body>: any ancestor with `backdrop-filter`, `transform`,
  // `filter`, `perspective`, `will-change`, or `contain` becomes the
  // containing block for `position: fixed` descendants — overriding the
  // viewport. The PlayerBar uses `backdrop-filter: blur(20px)` and would
  // otherwise re-anchor this popover off-screen. Portaling sidesteps the
  // trap entirely and also flattens z-index across all consumers.
  return createPortal(
    <div
      ref={ref}
      className="row-menu"
      role="menu"
      style={{
        left: coords.left,
        ...("top" in coords ? { top: coords.top } : { bottom: coords.bottom }),
      }}
    >
      {view === "root" && (
        <>
          {showQueueActions && (
            <>
              <button className="row-menu-item" onClick={playNext}>
                <Plus size={14} strokeWidth={1.5} />
                <span>play next</span>
              </button>
              <button className="row-menu-item" onClick={addToQueue}>
                <Plus size={14} strokeWidth={1.5} />
                <span>add to queue</span>
              </button>
            </>
          )}
          <button
            className="row-menu-item"
            onClick={() => setView("playlists")}
          >
            <ListPlus size={14} strokeWidth={1.5} />
            <span>add to playlist…</span>
            <ChevronRight size={14} strokeWidth={1.5} className="ml-auto" />
          </button>
          <div className="row-menu-sep" />
          <button className="row-menu-item" onClick={toggleDownload} disabled={busy}>
            {downloaded ? (
              <Trash2 size={14} strokeWidth={1.5} />
            ) : (
              <Download size={14} strokeWidth={1.5} />
            )}
            <span>{downloaded ? "remove download" : "save for offline"}</span>
          </button>
          {(track.albumId || track.artistId) && (
            <div className="row-menu-sep" />
          )}
          {track.albumId && (
            <button
              className="row-menu-item"
              onClick={() => {
                navigate(`/albums/${track.albumId}`);
                onClose();
              }}
            >
              <Disc3 size={14} strokeWidth={1.5} />
              <span>go to album</span>
            </button>
          )}
          {/* Subsonic returns artistId on most song rows, but it's optional
              in the type — fall through silently when absent. */}
          {track.artistId && (
            <button
              className="row-menu-item"
              onClick={() => {
                navigate(`/artists/${track.artistId}`);
                onClose();
              }}
            >
              <User size={14} strokeWidth={1.5} />
              <span>go to artist</span>
            </button>
          )}
        </>
      )}
      {view === "playlists" && (
        <PlaylistPicker
          onPick={addToExisting}
          onCreateNew={newPlaylistAndAdd}
          onBack={() => setView("root")}
        />
      )}
    </div>,
    document.body
  );
}

function PlaylistPicker({
  onPick,
  onCreateNew,
  onBack,
}: {
  onPick: (id: string) => void;
  onCreateNew: () => void;
  onBack: () => void;
}) {
  const q = useQuery({
    queryKey: ["playlists"],
    queryFn: listPlaylists,
    staleTime: 30_000,
  });
  return (
    <>
      <button className="row-menu-item is-back" onClick={onBack}>
        <ChevronRight
          size={14}
          strokeWidth={1.5}
          style={{ transform: "rotate(180deg)" }}
        />
        <span>back</span>
      </button>
      <div className="row-menu-sep" />
      <button className="row-menu-item" onClick={onCreateNew}>
        <Plus size={14} strokeWidth={1.5} />
        <span>new playlist…</span>
      </button>
      {q.isLoading && (
        <div className="row-menu-meta">loading…</div>
      )}
      {q.data && q.data.length > 0 && <div className="row-menu-sep" />}
      {q.data &&
        q.data.map((p) => (
          <button
            key={p.id}
            className="row-menu-item"
            onClick={() => onPick(p.id)}
          >
            <ListPlus size={14} strokeWidth={1.5} />
            <span className="truncate">{p.name}</span>
          </button>
        ))}
    </>
  );
}
