// Authenticated fetch wrapper.
//
// On 401 we attempt a single token refresh and retry once. If the retry
// also 401s the user is signed out — the caller can detect this and
// route to login.

import { refreshTokens } from "../auth/oauth";
import { clearTokens, readTokens } from "../auth/tokens";
import { Album, AlbumWithTracks, Track } from "./types";

class AuthError extends Error {}

async function apiFetch(path: string): Promise<Response> {
  const tokens = readTokens();
  if (!tokens) throw new AuthError("not signed in");

  const doFetch = (token: string) =>
    fetch(path, {
      headers: { Authorization: `Bearer ${token}` },
    });

  let res = await doFetch(tokens.accessToken);
  if (res.status === 401) {
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

// Subsonic responses are wrapped: { "subsonic-response": { status, ..., <key>: ... } }
async function getSubsonic<T>(path: string, key: string): Promise<T> {
  const sep = path.includes("?") ? "&" : "?";
  const res = await apiFetch(`${path}${sep}f=json&v=1.16.1&c=music-web`);
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  const json = (await res.json()) as { "subsonic-response": Record<string, unknown> };
  const env = json["subsonic-response"];
  if (env.status !== "ok") {
    const err = (env.error as { message?: string } | undefined)?.message ?? "subsonic error";
    throw new Error(err);
  }
  return env[key] as T;
}

export async function listAlbums(opts: { type: string; size: number }): Promise<Album[]> {
  const path = `/rest/getAlbumList2?type=${encodeURIComponent(opts.type)}&size=${opts.size}`;
  const result = await getSubsonic<{ album?: Album[] }>(path, "albumList2");
  return result.album ?? [];
}

export async function getAlbum(id: string): Promise<AlbumWithTracks> {
  const path = `/rest/getAlbum?id=${encodeURIComponent(id)}`;
  const raw = await getSubsonic<Album & { song?: Track[] }>(path, "album");
  const { song, ...album } = raw;
  return { album, tracks: song ?? [] };
}

export function streamUrl(trackId: string): string {
  // Browser <audio> can't add a Bearer header — pass the access token
  // as a query param. The gateway middleware accepts either Authorization
  // header OR ?access_token= for media URLs; see auth.rs comment.
  const tokens = readTokens();
  const auth = tokens ? `&access_token=${encodeURIComponent(tokens.accessToken)}` : "";
  return `/rest/stream?id=${encodeURIComponent(trackId)}${auth}`;
}

export function coverArtUrl(coverArt: string | undefined, size = 300): string | null {
  if (!coverArt) return null;
  const tokens = readTokens();
  const auth = tokens ? `&access_token=${encodeURIComponent(tokens.accessToken)}` : "";
  return `/rest/getCoverArt?id=${encodeURIComponent(coverArt)}&size=${size}${auth}`;
}

export { AuthError };
