// ⋯ menu for a track row. The popover mechanics — placement, portalling,
// dismissal — live in <RowMenu>; this file is just the action list.
//
// Submenu pattern: "add to playlist" replaces the menu body with the
// picker rather than fanning out a second floating panel. Because
// RowMenu mounts this body only while open, the `view` state resets on
// every open for free.

import {
  ChevronRight,
  Disc3,
  Download,
  ListPlus,
  Plus,
  ThumbsDown,
  ThumbsUp,
  Trash2,
  User,
} from "lucide-react";
import { useCallback, useState } from "react";
import { useAudioCache } from "../cache/AudioCacheContext";
import { formatBytes } from "../cache/format";
import { useEntityRating } from "../player/useRatings";
import { navigate } from "../router";
import { useSync } from "../sync/SyncContext";
import { useToast } from "../toast/ToastContext";
import { PlaylistPicker, usePlaylistAdd } from "./PlaylistPicker";
import {
  RowMenu,
  RowMenuExtras,
  RowMenuItem,
  RowMenuSep,
  type RowMenuExtraItem,
} from "./RowMenu";
import type { Track } from "../api/types";

export type { RowMenuExtraItem };

/**
 * @param showQueueActions  Whether to render "play next" / "add to queue".
 *   Default `true`. Set to `false` for rows whose track is *already* in
 *   the queue (the Queue page) — those actions don't make sense there.
 * @param extraItems  Optional caller-supplied entries prepended to the
 *   root view (the menu closes itself after invoking one).
 */
export function TrackRowMenu({
  track,
  showQueueActions = true,
  extraItems,
}: {
  track: Track;
  showQueueActions?: boolean;
  extraItems?: RowMenuExtraItem[] | undefined;
}) {
  return (
    <RowMenu label="track options">
      {(close) => (
        <TrackMenuBody
          track={track}
          showQueueActions={showQueueActions}
          extraItems={extraItems}
          onClose={close}
        />
      )}
    </RowMenu>
  );
}

function TrackMenuBody({
  track,
  showQueueActions,
  extraItems,
  onClose,
}: {
  track: Track;
  showQueueActions: boolean;
  extraItems: RowMenuExtraItem[] | undefined;
  onClose: () => void;
}) {
  const [view, setView] = useState<"root" | "playlists">("root");
  const [busy, setBusy] = useState(false);
  const sync = useSync();
  const toast = useToast();
  const cache = useAudioCache();
  const downloaded = cache.isDownloaded(track.id);
  // Durable taste signal — the same optimistic hook the player bar uses.
  // On phones (≤900px) the player's rating pills are hidden, so this menu
  // is the only way to like/dislike a track at all.
  const rating = useEntityRating("track", track.id);
  const playlist = usePlaylistAdd(
    useCallback(async () => [track.id], [track.id]),
    onClose,
  );

  if (view === "playlists") {
    return (
      <PlaylistPicker
        onPick={(id, name) => void playlist.addToExisting(id, name)}
        onCreateNew={() => void playlist.createAndAdd()}
        onBack={() => setView("root")}
      />
    );
  }

  async function toggleDownload() {
    setBusy(true);
    try {
      if (downloaded) {
        await cache.removeDownload(track.id);
        toast("download removed");
      } else {
        const outcome = await cache.download(track.id);
        if (outcome.kind === "would-exceed-budget") {
          toast(
            `Not enough offline space — short by ${formatBytes(outcome.overBy)}. ` +
              `Raise the download budget in Settings.`,
            { variant: "error" },
          );
        } else {
          toast("saved for offline", { variant: "success" });
        }
      }
    } catch (e) {
      toast(`couldn't update download: ${(e as Error).message}`, {
        variant: "error",
      });
    } finally {
      setBusy(false);
      onClose();
    }
  }

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
      toast("playing next", { variant: "success" });
    }
    onClose();
  }

  function addToQueue() {
    sync.pushTrack(track);
    toast("added to queue", { variant: "success" });
    onClose();
  }

  return (
    <>
      <RowMenuExtras items={extraItems} onClose={onClose} />
      {showQueueActions && (
        <>
          <RowMenuItem
            icon={<Plus size={14} strokeWidth={1.5} />}
            onClick={playNext}
          >
            play next
          </RowMenuItem>
          <RowMenuItem
            icon={<Plus size={14} strokeWidth={1.5} />}
            onClick={addToQueue}
          >
            add to queue
          </RowMenuItem>
          <RowMenuSep />
        </>
      )}
      <RowMenuItem
        icon={
          <ThumbsUp
            size={14}
            strokeWidth={1.5}
            fill={rating.rating === "like" ? "currentColor" : "none"}
          />
        }
        disabled={rating.pending}
        onClick={() => {
          rating.set("like");
          onClose();
        }}
      >
        {rating.rating === "like" ? "remove like" : "like"}
      </RowMenuItem>
      <RowMenuItem
        icon={
          <ThumbsDown
            size={14}
            strokeWidth={1.5}
            fill={rating.rating === "dislike" ? "currentColor" : "none"}
          />
        }
        disabled={rating.pending}
        onClick={() => {
          rating.set("dislike");
          onClose();
        }}
      >
        {rating.rating === "dislike" ? "remove dislike" : "dislike"}
      </RowMenuItem>
      <RowMenuSep />
      <RowMenuItem
        icon={<ListPlus size={14} strokeWidth={1.5} />}
        trailing={<ChevronRight size={14} strokeWidth={1.5} />}
        onClick={() => setView("playlists")}
      >
        add to playlist…
      </RowMenuItem>
      <RowMenuSep />
      <RowMenuItem
        icon={
          downloaded ? (
            <Trash2 size={14} strokeWidth={1.5} />
          ) : (
            <Download size={14} strokeWidth={1.5} />
          )
        }
        disabled={busy}
        onClick={() => void toggleDownload()}
      >
        {downloaded ? "remove download" : "save for offline"}
      </RowMenuItem>
      {(track.albumId || track.artistId) && <RowMenuSep />}
      {track.albumId && (
        <RowMenuItem
          icon={<Disc3 size={14} strokeWidth={1.5} />}
          onClick={() => {
            navigate(`/albums/${track.albumId}`);
            onClose();
          }}
        >
          go to album
        </RowMenuItem>
      )}
      {/* Subsonic returns artistId on most song rows, but it's optional
          in the type — fall through silently when absent. */}
      {track.artistId && (
        <RowMenuItem
          icon={<User size={14} strokeWidth={1.5} />}
          onClick={() => {
            navigate(`/artists/${track.artistId}`);
            onClose();
          }}
        >
          go to artist
        </RowMenuItem>
      )}
    </>
  );
}
