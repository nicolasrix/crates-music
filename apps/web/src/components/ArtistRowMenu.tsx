// ⋯ menu for an artist — search results (rows and hero cards) and the
// similar-artists strip on the artist page.
//
// Deliberately shorter than the album menu. An artist has no single
// canonical tracklist: `artistTracks` synthesises one from top songs (or
// a bounded slice of the discography), which is right for "play" and
// "queue" but would be a surprising thing to silently pin to disk or
// paste into a playlist. Those stay album-level actions.

import { useQueryClient } from "@tanstack/react-query";
import { Play, Plus, Radio, ThumbsDown, ThumbsUp, User } from "lucide-react";
import { useCallback, useState } from "react";
import { SeedNotEmbeddedError, startStationFromAny } from "../api/recommend";
import { useEntityRating } from "../player/useRatings";
import { navigate } from "../router";
import { artistTracks } from "../sync/artistTracks";
import { useSync } from "../sync/SyncContext";
import { usePlayback } from "../sync/usePlayback";
import { useToast } from "../toast/ToastContext";
import { RowMenu, RowMenuItem, RowMenuSep } from "./RowMenu";
import type { Artist, Track } from "../api/types";

const STATION_N = 20;
/** Seed candidates handed to the station. The whole tracklist would be a
 *  long URL for no gain — the endpoint takes the first embedded seed it
 *  finds, and if none of the artist's top dozen tracks are indexed, the
 *  rest almost certainly aren't either. */
const STATION_SEEDS = 12;

export function ArtistRowMenu({ artist }: { artist: Artist }) {
  return (
    <RowMenu label={`options for ${artist.name}`}>
      {(close) => <ArtistMenuBody artist={artist} onClose={close} />}
    </RowMenu>
  );
}

function ArtistMenuBody({
  artist,
  onClose,
}: {
  artist: Artist;
  onClose: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const queryClient = useQueryClient();
  const sync = useSync();
  const toast = useToast();
  const { playList } = usePlayback();
  const rating = useEntityRating("artist", artist.id);

  const loadTracks = useCallback(
    (): Promise<Track[]> => artistTracks(queryClient, artist),
    [queryClient, artist],
  );

  async function playArtist() {
    setBusy(true);
    try {
      const tracks = await loadTracks();
      if (tracks.length === 0) {
        toast("no playable tracks for this artist", { variant: "error" });
        return;
      }
      playList(tracks, 0);
    } catch (e) {
      toast(`couldn't play artist: ${(e as Error).message}`, {
        variant: "error",
      });
    } finally {
      setBusy(false);
      onClose();
    }
  }

  async function addToQueue() {
    setBusy(true);
    try {
      const tracks = await loadTracks();
      for (const t of tracks) sync.pushTrack(t);
      toast(`added ${tracks.length} tracks to queue`, { variant: "success" });
    } catch (e) {
      toast(`couldn't queue artist: ${(e as Error).message}`, {
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
      const seeds = (await loadTracks()).slice(0, STATION_SEEDS);
      const { tracks } = await startStationFromAny(
        seeds.map((t) => t.id),
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
          ? "this artist isn't indexed for recommendations yet"
          : `couldn't start station: ${(e as Error).message}`,
        { variant: "error" },
      );
    } finally {
      setBusy(false);
      onClose();
    }
  }

  return (
    <>
      <RowMenuItem
        icon={<Play size={14} strokeWidth={1.5} />}
        disabled={busy}
        onClick={() => void playArtist()}
      >
        play artist
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
        icon={<User size={14} strokeWidth={1.5} />}
        onClick={() => {
          navigate(`/artists/${artist.id}`);
          onClose();
        }}
      >
        go to artist
      </RowMenuItem>
    </>
  );
}
