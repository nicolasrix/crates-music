// The "add to playlist…" submenu, shared by the track / album / artist row
// menus. Rendered *in place of* the root menu body rather than as a second
// floating panel: one pane is plenty for the actions we have, and nested
// fixed-position elements fight both outside-click detection and the
// viewport clamping in rowMenuCoords.
//
// The picker is presentational; `usePlaylistAdd` owns the mutation so all
// three menus report success, failure, and cache invalidation identically.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronRight, ListPlus, Plus } from "lucide-react";
import { useCallback, useRef } from "react";
import {
  addTracksToPlaylist,
  createPlaylist,
  listPlaylists,
  type PlaylistAddResult,
} from "../api/playlists";
import { useToast } from "../toast/ToastContext";
import { playlistAddMessage } from "../utils/playlistAddMessage";
import { RowMenuItem, RowMenuSep } from "./RowMenu";

/** Resolves the tracks to add, at click time. Album and artist menus only
 *  know an id up front, so the fetch has to be deferred until the user
 *  actually picks a destination — opening the submenu must stay free. */
export type ResolveTrackIds = () => Promise<readonly string[]>;

export interface PlaylistAdd {
  addToExisting: (playlistId: string, playlistName: string) => Promise<void>;
  createAndAdd: () => Promise<void>;
}

export function usePlaylistAdd(
  resolveTrackIds: ResolveTrackIds,
  onDone: () => void,
): PlaylistAdd {
  const queryClient = useQueryClient();
  const toast = useToast();

  // The resolver closes over props and so is a fresh function every
  // render; keeping it in a ref lets the callbacks below stay stable
  // without the caller having to memoise anything.
  const resolveRef = useRef(resolveTrackIds);
  resolveRef.current = resolveTrackIds;
  const doneRef = useRef(onDone);
  doneRef.current = onDone;

  // The gateway skips ids the playlist already holds, so what the user is
  // told comes from *its* counts, never from how many we submitted.
  const report = useCallback(
    (name: string, result: PlaylistAddResult) => {
      const { message, variant } = playlistAddMessage(name, result);
      toast(message, { variant });
    },
    [toast],
  );

  const addToExisting = useCallback(
    async (playlistId: string, playlistName: string) => {
      try {
        const ids = await resolveRef.current();
        if (ids.length === 0) {
          toast("nothing to add", { variant: "error" });
          return;
        }
        const result = await addTracksToPlaylist(playlistId, ids);
        await queryClient.invalidateQueries({ queryKey: ["playlists"] });
        await queryClient.invalidateQueries({ queryKey: ["playlist", playlistId] });
        report(playlistName, result);
      } catch (e) {
        toast(`couldn't add to playlist: ${(e as Error).message}`, {
          variant: "error",
        });
      } finally {
        doneRef.current();
      }
    },
    [queryClient, report, toast],
  );

  const createAndAdd = useCallback(async () => {
    const raw = window.prompt("playlist name");
    if (raw === null) return;
    const name = raw.trim();
    if (name.length === 0) return;
    try {
      const ids = await resolveRef.current();
      const created = await createPlaylist(name);
      const result = created.id
        ? await addTracksToPlaylist(created.id, ids)
        : { added: 0, skipped: 0 };
      await queryClient.invalidateQueries({ queryKey: ["playlists"] });
      // Still create the playlist the user asked for when the source
      // turns out to be empty — just don't claim we filled it. (A brand
      // new playlist can't have duplicates, so `skipped` is always 0.)
      if (ids.length === 0) toast(`created “${name}” — nothing to add`);
      else report(name, result);
    } catch (e) {
      toast(`couldn't create playlist: ${(e as Error).message}`, {
        variant: "error",
      });
    } finally {
      doneRef.current();
    }
  }, [queryClient, report, toast]);

  return { addToExisting, createAndAdd };
}

export function PlaylistPicker({
  onPick,
  onCreateNew,
  onBack,
}: {
  onPick: (id: string, name: string) => void;
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
      <RowMenuItem
        className="is-back"
        icon={
          <ChevronRight
            size={14}
            strokeWidth={1.5}
            style={{ transform: "rotate(180deg)" }}
          />
        }
        onClick={onBack}
      >
        back
      </RowMenuItem>
      <RowMenuSep />
      <RowMenuItem
        icon={<Plus size={14} strokeWidth={1.5} />}
        onClick={onCreateNew}
      >
        new playlist…
      </RowMenuItem>
      {q.isLoading && <div className="row-menu-meta">loading…</div>}
      {q.data && q.data.length > 0 && <RowMenuSep />}
      {q.data?.map((p) => (
        <RowMenuItem
          key={p.id}
          icon={<ListPlus size={14} strokeWidth={1.5} />}
          onClick={() => onPick(p.id, p.name)}
        >
          {p.name}
        </RowMenuItem>
      ))}
    </>
  );
}
