// Wrapper for the gateway's gateway-owned playlists (`/v1/playlists/*`,
// user-system PR F / decision D6).
//
// Playlists moved OFF Navidrome's `/rest/*` so membership can be private
// per-user. Navidrome stays catalog-only: a playlist holds only track ids,
// so `getPlaylist` hydrates them into full `Track`s via `getSong` against
// the shared catalog. The function names + return shapes are unchanged
// from the old Subsonic-backed versions, so consumers (Sidebar, the
// Playlist page, TrackRowMenu) only swapped their import path.
//
// Auth piggybacks on the same Bearer-with-refresh dance as events.ts /
// library.ts. Copied locally for the same reason noted there (client.ts's
// apiFetch is GET-only and Subsonic-envelope-focused).

import { refreshTokens } from "../auth/oauth";
import { clearTokens, readTokens } from "../auth/tokens";
import { getSong } from "./client";
import type { PlaylistSummary, PlaylistWithTracks, Track } from "./types";

class AuthError extends Error {}

async function apiFetch(path: string, init?: RequestInit): Promise<Response> {
  const tokens = readTokens();
  if (!tokens) throw new AuthError("not signed in");
  const doFetch = (token: string) =>
    fetch(path, {
      ...init,
      headers: { ...(init?.headers ?? {}), Authorization: `Bearer ${token}` },
    });

  let res = await doFetch(tokens.accessToken);
  if (res.status === 401) {
    if (!tokens.refreshToken) {
      clearTokens();
      throw new AuthError("guest session expired");
    }
    try {
      await refreshTokens(tokens.refreshToken);
    } catch {
      clearTokens();
      throw new AuthError("session expired");
    }
    const refreshed = readTokens();
    if (!refreshed) throw new AuthError("session expired");
    res = await doFetch(refreshed.accessToken);
    if (res.status === 401) {
      clearTokens();
      throw new AuthError("session expired");
    }
  }
  return res;
}

// The gateway's snake_case wire row. `owned` is relative to the caller so
// the UI can show edit controls only on the caller's own playlists.
interface PlaylistWire {
  id: string;
  name: string;
  visibility: "private" | "shared";
  owner_user_id: number;
  owned: boolean;
  song_count: number;
  created_ms: number;
  updated_ms: number;
}

function toSummary(w: PlaylistWire): PlaylistSummary {
  return { id: w.id, name: w.name, songCount: w.song_count };
}

export async function listPlaylists(): Promise<PlaylistSummary[]> {
  const res = await apiFetch("/v1/playlists");
  if (!res.ok) throw new Error(`playlists HTTP ${res.status}`);
  const body = (await res.json()) as { playlists: PlaylistWire[] };
  return body.playlists.map(toSummary);
}

export async function getPlaylist(id: string): Promise<PlaylistWithTracks> {
  const res = await apiFetch(`/v1/playlists/${encodeURIComponent(id)}`);
  if (!res.ok) throw new Error(`playlist HTTP ${res.status}`);
  const body = (await res.json()) as {
    playlist: PlaylistWire;
    track_ids: string[];
  };
  // Catalog stays on Navidrome — hydrate ids → tracks against the shared
  // catalog. A track removed from Navidrome resolves to null and is
  // dropped rather than failing the whole playlist load.
  const settled = await Promise.allSettled(body.track_ids.map((tid) => getSong(tid)));
  const tracks = settled
    .filter((r): r is PromiseFulfilledResult<Track> => r.status === "fulfilled")
    .map((r) => r.value);
  // Keep the raw ordered ids alongside the hydrated tracks — membership
  // edits (remove/reorder) replace against these, so an id that failed to
  // hydrate this load isn't dropped from the stored playlist.
  return { playlist: toSummary(body.playlist), tracks, trackIds: body.track_ids };
}

export async function createPlaylist(name: string): Promise<PlaylistSummary> {
  const res = await apiFetch("/v1/playlists", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name }),
  });
  if (!res.ok) throw new Error(`createPlaylist HTTP ${res.status}`);
  return toSummary((await res.json()) as PlaylistWire);
}

// What an append actually did. The gateway de-duplicates in append mode,
// so `skipped` is how many of the submitted ids were already members —
// the number the UI turns into "already in this playlist".
export interface PlaylistAddResult {
  added: number;
  skipped: number;
}

// Append songs to a playlist, in the order given. One request regardless
// of count — the endpoint has always taken a list, so adding a whole album
// or artist costs the same round trip as adding one track. De-duplication
// against current membership is the *server's* job (it has to be: only the
// gateway can check-and-insert atomically), and it reports back the split.
export async function addTracksToPlaylist(
  playlistId: string,
  trackIds: readonly string[],
): Promise<PlaylistAddResult> {
  if (trackIds.length === 0) return { added: 0, skipped: 0 };
  const res = await apiFetch(`/v1/playlists/${encodeURIComponent(playlistId)}/tracks`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ track_ids: trackIds, mode: "append" }),
  });
  if (!res.ok) throw new Error(`addTracksToPlaylist HTTP ${res.status}`);
  return (await readAddResult(res)) ?? { added: trackIds.length, skipped: 0 };
}

// A gateway from before the counts existed answers 204 with no body. Fall
// back to "everything was added" there rather than throwing — the write
// itself succeeded, we just can't say whether anything was a duplicate.
async function readAddResult(res: Response): Promise<PlaylistAddResult | null> {
  if (res.status === 204) return null;
  try {
    const body = (await res.json()) as Partial<PlaylistAddResult>;
    if (typeof body.added !== "number" || typeof body.skipped !== "number") return null;
    return { added: body.added, skipped: body.skipped };
  } catch {
    return null;
  }
}

// Append a single song to a playlist (the row-menu "add to playlist").
export async function addTrackToPlaylist(
  playlistId: string,
  trackId: string,
): Promise<PlaylistAddResult> {
  return addTracksToPlaylist(playlistId, [trackId]);
}

// Replace a playlist's whole membership (reorder / remove). Positions are
// taken from array order.
export async function setPlaylistTracks(playlistId: string, trackIds: string[]): Promise<void> {
  const res = await apiFetch(`/v1/playlists/${encodeURIComponent(playlistId)}/tracks`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ track_ids: trackIds, mode: "replace" }),
  });
  if (!res.ok) throw new Error(`setPlaylistTracks HTTP ${res.status}`);
}

// Remove every occurrence of a track id from a playlist. `currentTrackIds`
// MUST be the raw stored ids (`PlaylistWithTracks.trackIds`), not the
// hydrated `tracks` — replacing against the hydrated subset would delete any
// id that failed to resolve against the catalog this load. Returns the new
// id list so callers can update their optimistic cache with the same value.
export async function removeTrackFromPlaylist(
  playlistId: string,
  trackId: string,
  currentTrackIds: readonly string[],
): Promise<string[]> {
  const next = currentTrackIds.filter((t) => t !== trackId);
  await setPlaylistTracks(playlistId, next);
  return next;
}

export async function renamePlaylist(playlistId: string, name: string): Promise<void> {
  const res = await apiFetch(`/v1/playlists/${encodeURIComponent(playlistId)}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name }),
  });
  if (!res.ok) throw new Error(`renamePlaylist HTTP ${res.status}`);
}

export async function deletePlaylist(playlistId: string): Promise<void> {
  const res = await apiFetch(`/v1/playlists/${encodeURIComponent(playlistId)}`, {
    method: "DELETE",
  });
  if (!res.ok) throw new Error(`deletePlaylist HTTP ${res.status}`);
}

export { AuthError };
