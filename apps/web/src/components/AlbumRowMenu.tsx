// ⋯ menu for an album — search results (rows and hero cards) and the
// albums grid. Everything here needs the album's tracklist, which the
// surfaces that render an album *don't* have, so each action resolves it
// on click through the shared `["album", id]` query key. That key is the
// album page's own, so a warm cache makes this a microtask and the
// click's transient user activation survives into audio.play().

import { useQueryClient } from "@tanstack/react-query";
import {
  Disc3,
  Download,
  ListPlus,
  Play,
  Plus,
  Radio,
  ThumbsDown,
  ThumbsUp,
  User,
} from "lucide-react";
import { useCallback, useState } from "react";
import { getAlbum } from "../api/client";
import { SeedNotEmbeddedError, startStationFromAny } from "../api/recommend";
import { useAudioCache } from "../cache/AudioCacheContext";
import { downloadTracks } from "../cache/downloadTracks";
import { formatBytes } from "../cache/format";
import { useEntityRating } from "../player/useRatings";
import { navigate } from "../router";
import { useSync } from "../sync/SyncContext";
import { usePlayback } from "../sync/usePlayback";
import { useToast } from "../toast/ToastContext";
import { PlaylistPicker, usePlaylistAdd } from "./PlaylistPicker";
import { RowMenu, RowMenuItem, RowMenuSep } from "./RowMenu";
import type { Album, Track } from "../api/types";

const STATION_N = 20;

export function AlbumRowMenu({ album }: { album: Album }) {
  return (
    <RowMenu label={`options for ${album.name}`}>
      {(close) => <AlbumMenuBody album={album} onClose={close} />}
    </RowMenu>
  );
}

function AlbumMenuBody({
  album,
  onClose,
}: {
  album: Album;
  onClose: () => void;
}) {
  const [view, setView] = useState<"root" | "playlists">("root");
  const [busy, setBusy] = useState(false);
  const queryClient = useQueryClient();
  const sync = useSync();
  const toast = useToast();
  const cache = useAudioCache();
  const { playAlbum, playList } = usePlayback();
  const rating = useEntityRating("album", album.id);

  const loadTracks = useCallback(async (): Promise<Track[]> => {
    const { tracks } = await queryClient.fetchQuery({
      queryKey: ["album", album.id],
      queryFn: () => getAlbum(album.id),
      staleTime: 5 * 60_000,
    });
    return tracks;
  }, [queryClient, album.id]);

  const playlist = usePlaylistAdd(
    async () => (await loadTracks()).map((t) => t.id),
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

  async function addToQueue() {
    setBusy(true);
    try {
      const tracks = await loadTracks();
      // pushTrack per track rather than a queue-replacing start_session:
      // "add to queue" must not disturb what's playing.
      for (const t of tracks) sync.pushTrack(t);
      toast(`added ${tracks.length} tracks to queue`, { variant: "success" });
    } catch (e) {
      toast(`couldn't queue album: ${(e as Error).message}`, {
        variant: "error",
      });
    } finally {
      setBusy(false);
      onClose();
    }
  }

  async function startStation() {
    setBusy(true);
    try {
      const albumTracks = await loadTracks();
      // Walk the album's tracks in order; the first one already in the
      // ANN seeds the station. Only call the album unindexed if every
      // track 404s — a single unembedded track is normal at our ingest
      // coverage.
      const { tracks } = await startStationFromAny(
        albumTracks.map((t) => t.id),
        STATION_N,
      );
      if (tracks.length === 0) {
        toast("station came back empty", { variant: "error" });
        return;
      }
      playList(tracks, 0);
    } catch (e) {
      toast(
        e instanceof SeedNotEmbeddedError
          ? "this album isn't indexed for recommendations yet"
          : `couldn't start station: ${(e as Error).message}`,
        { variant: "error" },
      );
    } finally {
      setBusy(false);
      onClose();
    }
  }

  async function saveOffline() {
    setBusy(true);
    try {
      const tracks = await loadTracks();
      const r = await downloadTracks(cache, tracks.map((t) => t.id));
      if (r.shortBy !== null) {
        toast(
          `Download budget full — short by ${formatBytes(r.shortBy)}. ` +
            `Raise it in Settings to finish.`,
          { variant: "error" },
        );
      } else if (r.failed > 0) {
        toast(`saved ${r.saved}/${tracks.length} — ${r.failed} failed`, {
          variant: "error",
        });
      } else {
        toast(`${r.saved} tracks saved for offline`, { variant: "success" });
      }
    } catch (e) {
      toast(`couldn't download album: ${(e as Error).message}`, {
        variant: "error",
      });
    } finally {
      setBusy(false);
      onClose();
    }
  }

  return (
    <>
      <RowMenuItem
        icon={<Play size={14} strokeWidth={1.5} />}
        onClick={() => {
          void playAlbum(album.id);
          onClose();
        }}
      >
        play album
      </RowMenuItem>
      <RowMenuItem
        icon={<Plus size={14} strokeWidth={1.5} />}
        disabled={busy}
        onClick={() => void addToQueue()}
      >
        add to queue
      </RowMenuItem>
      <RowMenuItem
        icon={<Radio size={14} strokeWidth={1.5} />}
        disabled={busy}
        onClick={() => void startStation()}
      >
        start station
      </RowMenuItem>
      <RowMenuSep />
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
        onClick={() => setView("playlists")}
      >
        add to playlist…
      </RowMenuItem>
      <RowMenuItem
        icon={<Download size={14} strokeWidth={1.5} />}
        disabled={busy}
        onClick={() => void saveOffline()}
      >
        save album for offline
      </RowMenuItem>
      <RowMenuSep />
      <RowMenuItem
        icon={<Disc3 size={14} strokeWidth={1.5} />}
        onClick={() => {
          navigate(`/albums/${album.id}`);
          onClose();
        }}
      >
        go to album
      </RowMenuItem>
      {album.artistId && (
        <RowMenuItem
          icon={<User size={14} strokeWidth={1.5} />}
          onClick={() => {
            navigate(`/artists/${album.artistId}`);
            onClose();
          }}
        >
          go to artist
        </RowMenuItem>
      )}
    </>
  );
}
