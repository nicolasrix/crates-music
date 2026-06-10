// Authenticated fetch wrapper.
//
// On 401 we attempt a single token refresh and retry once. If the retry
// also 401s the user is signed out — the caller can detect this and
// route to login.

import { refreshTokens } from "../auth/oauth";
import { clearTokens, readTokens } from "../auth/tokens";
import { codecFromContentType } from "../cache/audioKey";
import { streamQualityParams } from "../settings/playback";
import {
  Album,
  AlbumWithTracks,
  Artist,
  ArtistWithAlbums,
  Track,
} from "./types";

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
    // Guest sessions (PR D) carry no refresh token — a 401 is terminal,
    // the visitor must re-redeem the code. Don't attempt a refresh.
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

// Subsonic's per-request cap. The spec mandates servers SHOULD honour up to
// 500 per call; Navidrome enforces this exactly. To get more than 500 you
// have to paginate via offset — see listAllAlbums.
const ALBUMLIST_PAGE_MAX = 500;

export async function listAlbums(opts: {
  type: string;
  size: number;
  offset?: number;
}): Promise<Album[]> {
  const params = new URLSearchParams({
    type: opts.type,
    size: String(opts.size),
  });
  if (opts.offset !== undefined) params.set("offset", String(opts.offset));
  const path = `/rest/getAlbumList2?${params.toString()}`;
  const result = await getSubsonic<{ album?: Album[] }>(path, "albumList2");
  return result.album ?? [];
}

// Fetches every album in the library by repeatedly calling getAlbumList2
// with increasing offset. The terminating signal is a short page (fewer
// items than the per-request cap) — Subsonic has no "total count" field,
// so you discover the tail empirically.
//
// Sequential rather than parallel: page count is small at single-user
// scale (a few thousand albums → ~10 round-trips), HTTP/2 keeps the
// connection warm, and avoiding parallel bursts is friendlier to the
// gateway's L2 metadata cache and its upstream.
export async function listAllAlbums(
  type = "alphabeticalByName"
): Promise<Album[]> {
  const out: Album[] = [];
  let offset = 0;
  // Hard ceiling to guarantee termination if the server ever misbehaves
  // and keeps returning full pages forever.
  const MAX_PAGES = 200;
  for (let page = 0; page < MAX_PAGES; page++) {
    const chunk = await listAlbums({
      type,
      size: ALBUMLIST_PAGE_MAX,
      offset,
    });
    out.push(...chunk);
    if (chunk.length < ALBUMLIST_PAGE_MAX) break;
    offset += ALBUMLIST_PAGE_MAX;
  }
  return out;
}

export async function getAlbum(id: string): Promise<AlbumWithTracks> {
  const path = `/rest/getAlbum?id=${encodeURIComponent(id)}`;
  const raw = await getSubsonic<Album & { song?: Track[] }>(path, "album");
  const { song, ...album } = raw;
  return { album, tracks: song ?? [] };
}

// Single-track lookup. Used to hydrate recommendation results, which return
// only track_ids — no metadata.
export async function getSong(id: string): Promise<Track> {
  return getSubsonic<Track>(
    `/rest/getSong?id=${encodeURIComponent(id)}`,
    "song"
  );
}

// Subsonic /rest/getArtists returns a nested shape: { index: [{ artist: [...] }] }.
// Flatten to a single Artist[] sorted by name (which the response already is).
export async function listArtists(): Promise<Artist[]> {
  type IndexBlock = { name: string; artist?: Artist[] };
  type ArtistsResp = { index?: IndexBlock[] };
  const result = await getSubsonic<ArtistsResp>("/rest/getArtists", "artists");
  return (result.index ?? []).flatMap((b) => b.artist ?? []);
}

export async function getArtist(id: string): Promise<ArtistWithAlbums> {
  const path = `/rest/getArtist?id=${encodeURIComponent(id)}`;
  const raw = await getSubsonic<Artist & { album?: Album[] }>(path, "artist");
  const { album, ...artist } = raw;
  return { artist, albums: album ?? [] };
}

// Playlists moved off `/rest/*` to the gateway-owned `/v1/playlists/*`
// store (user-system PR F). Their client wrappers — listPlaylists,
// getPlaylist, createPlaylist, addTrackToPlaylist, renamePlaylist,
// deletePlaylist, setPlaylistTracks — live in `./playlists`.

// "Recent tracks" — derived from the newest-albums endpoint, NOT from
// search3 directly. search3's empty-query result has no guaranteed order
// (and even when it looks sorted, the order won't match what /albums
// shows under "recently added"). Going through getAlbumList2?type=newest
// guarantees the two views agree.
//
// We over-estimate the album count (avg ~10 tracks/album) and trim the
// flattened list to `size`. Per-album track fetches run in parallel —
// the gateway caches each `getAlbum` aggressively, so warm-cache cost
// is effectively one round-trip.
export async function listRecentTracks(size = 200): Promise<Track[]> {
  const albumCount = Math.max(10, Math.ceil(size / 8));
  const albums = await listAlbums({ type: "newest", size: albumCount });
  // Promise.all preserves index order, so the flattened tracks come
  // out album-by-album in newest-first order without an extra sort.
  const details = await Promise.all(
    albums.map((a) => getAlbum(a.id).catch(() => null)),
  );
  const tracks: Track[] = [];
  for (const detail of details) {
    if (!detail) continue;
    for (const t of detail.tracks) {
      tracks.push(t);
      if (tracks.length >= size) return tracks;
    }
  }
  return tracks;
}

// Paginated track listing via Subsonic search3 with songOffset. Subsonic
// has no dedicated "list all songs" endpoint; an empty-query search3 is
// the conventional substitute. The server returns `song.length` rows
// (Navidrome's default order — roughly insertion order); a short page
// signals the tail (no `total` field is exposed).
export async function listTracksPage(opts: {
  size: number;
  offset?: number;
}): Promise<Track[]> {
  const params = new URLSearchParams({
    query: "",
    songCount: String(opts.size),
    albumCount: "0",
    artistCount: "0",
  });
  if (opts.offset !== undefined && opts.offset > 0) {
    params.set("songOffset", String(opts.offset));
  }
  const path = `/rest/search3?${params.toString()}`;
  const result = await getSubsonic<{ song?: Track[] }>(path, "searchResult3");
  return result.song ?? [];
}

export async function listRandomTracks(size = 100): Promise<Track[]> {
  type Resp = { song?: Track[] };
  const result = await getSubsonic<Resp>(
    `/rest/getRandomSongs?size=${size}`,
    "randomSongs"
  );
  return result.song ?? [];
}

// Most-played tracks — vanilla Subsonic has no global "top songs" endpoint
// (only per-artist via getTopSongs). Navidrome's OpenSubsonic exposes
// `playCount` on song rows, so the workaround is: pull a wide search3
// sample and sort client-side. Library scale is single-user, so a 1000-
// row sample is fine. Tracks with no `playCount` field are excluded —
// "most played" of zero plays would just be the search3 default order.
export async function listMostPlayedTracks(size = 100): Promise<Track[]> {
  type Resp = { song?: Track[] };
  const result = await getSubsonic<Resp>(
    `/rest/search3?query=${encodeURIComponent("")}&songCount=1000&albumCount=0&artistCount=0`,
    "searchResult3"
  );
  const songs = result.song ?? [];
  return songs
    .filter((s) => (s.playCount ?? 0) > 0)
    .sort((a, b) => (b.playCount ?? 0) - (a.playCount ?? 0))
    .slice(0, size);
}

export interface SearchResults {
  artists: Artist[];
  albums: Album[];
  tracks: Track[];
}

// Library-wide search via Subsonic search3. Caller decides per-section
// caps; the defaults match what the Search page renders without paging.
export async function searchAll(
  query: string,
  opts: { artistCount?: number; albumCount?: number; songCount?: number } = {}
): Promise<SearchResults> {
  const { artistCount = 20, albumCount = 40, songCount = 60 } = opts;
  const path =
    `/rest/search3?query=${encodeURIComponent(query)}` +
    `&artistCount=${artistCount}&albumCount=${albumCount}&songCount=${songCount}`;
  type Resp = { artist?: Artist[]; album?: Album[]; song?: Track[] };
  const result = await getSubsonic<Resp>(path, "searchResult3");
  return {
    artists: result.artist ?? [],
    albums: result.album ?? [],
    tracks: result.song ?? [],
  };
}

// Scrobble — Subsonic's /rest/scrobble endpoint. `submission=false` is a
// "now playing" ping, `submission=true` is a final play count. The
// gateway intercepts both: on submission it writes a fast-path
// last_played_ms row before forwarding to Navidrome (which remains the
// canonical play-count ledger). Fire-and-forget at the caller — a
// failed scrobble is not a UI-facing error, and the audio keeps playing.
export async function scrobble(
  trackId: string,
  submission: boolean
): Promise<void> {
  const params = new URLSearchParams({
    id: trackId,
    submission: submission ? "true" : "false",
    time: String(Date.now()),
    f: "json",
    v: "1.16.1",
    c: "music-web",
  });
  const res = await apiFetch(`/rest/scrobble?${params.toString()}`);
  if (!res.ok) throw new Error(`scrobble HTTP ${res.status}`);
}

export function streamUrl(trackId: string): string {
  // Browser <audio> can't add a Bearer header — pass the access token
  // as a query param. The gateway middleware accepts either Authorization
  // header OR ?access_token= for media URLs; see auth.rs comment.
  const tokens = readTokens();
  const auth = tokens ? `&access_token=${encodeURIComponent(tokens.accessToken)}` : "";
  // Streaming quality (Settings → Playback): transcode live playback to fit
  // a metered connection. Forwarded verbatim through the /rest proxy to
  // Navidrome. Independent of the offline-cache download quality; null =
  // passthrough original. Read at src-build time so a change applies to the
  // next track load.
  const q = streamQualityParams();
  const fmt = q ? `&format=${q.format}&maxBitRate=${q.maxBitRate}` : "";
  return `/rest/stream?id=${encodeURIComponent(trackId)}${fmt}${auth}`;
}

// Download a full track for the offline cache. Unlike streamUrl (which is
// consumed by an <audio> element and so passes ?access_token=), this goes
// through apiFetch with an Authorization header and token-refresh, and
// returns the bytes plus the codec the server actually served (derived
// from Content-Type — the web Track type carries no suffix). Caller stores
// it keyed by (trackId, bitrate, codec).
//
// `quality` (transcode-to-fit): forwarded verbatim through the gateway's
// /rest proxy to Navidrome's stream endpoint, which transcodes server-side
// — the same params the ingest fetcher uses. Omitted = passthrough original.
export async function fetchTrackBlob(
  trackId: string,
  quality?: { format: string; maxBitRate: number } | null,
): Promise<{ blob: Blob; codec: string }> {
  const params = new URLSearchParams({ id: trackId });
  if (quality) {
    params.set("format", quality.format);
    params.set("maxBitRate", String(quality.maxBitRate));
  }
  const res = await apiFetch(`/rest/stream?${params.toString()}`);
  if (!res.ok) throw new Error(`stream HTTP ${res.status}`);
  const blob = await res.blob();
  const codec = codecFromContentType(res.headers.get("content-type"));
  return { blob, codec };
}

export function coverArtUrl(
  coverArt: string | undefined,
  size = 300,
  seed?: string,
): string | null {
  if (!coverArt) return null;
  const tokens = readTokens();
  const auth = tokens ? `&access_token=${encodeURIComponent(tokens.accessToken)}` : "";
  // `seed` is a gateway-only hint that drives the placeholder initial when
  // Navidrome returns its built-in default image. Stripped before forwarding
  // upstream — see STRIPPED_PARAM_KEYS in crates/music-gateway/src/proxy.rs.
  const seedQ = seed ? `&seed=${encodeURIComponent(seed)}` : "";
  return `/rest/getCoverArt?id=${encodeURIComponent(coverArt)}&size=${size}${auth}${seedQ}`;
}

export type Role = "admin" | "user" | "guest";

export interface Whoami {
  user_id: number;
  role: Role;
  username: string | null;
  display_name: string | null;
  host_user_id: number | null;
}

/** Resolve the calling principal (identity + role) for role-gated UI. */
export async function whoami(): Promise<Whoami> {
  const res = await apiFetch("/v1/whoami");
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return (await res.json()) as Whoami;
}

// ---- Guest codes (PR D) — host-side management ------------------------

export interface GuestCode {
  id: number;
  label: string | null;
  created_at_unix_ms: number;
  expires_at_unix_ms: number | null;
  max_uses: number | null;
  uses: number;
  revoked_at_unix_ms: number | null;
}

export interface CreatedGuestCode {
  id: number;
  /** Plaintext — shown once at creation, never recoverable afterwards. */
  code: string;
  expires_at_unix_ms: number | null;
  max_uses: number | null;
}

/** List the caller's guest codes (newest first). Forbidden for guests. */
export async function listGuestCodes(): Promise<GuestCode[]> {
  const res = await apiFetch("/v1/guest_codes");
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return (await res.json()) as GuestCode[];
}

/** Mint a new guest code owned by the caller. Returns the plaintext once. */
export async function createGuestCode(opts: {
  label?: string | undefined;
  expiresInSeconds?: number | undefined;
  maxUses?: number | undefined;
}): Promise<CreatedGuestCode> {
  const body: Record<string, unknown> = {};
  if (opts.label) body.label = opts.label;
  if (opts.expiresInSeconds) body.expires_in_seconds = opts.expiresInSeconds;
  if (opts.maxUses) body.max_uses = opts.maxUses;
  const res = await apiFetch("/v1/guest_codes", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return (await res.json()) as CreatedGuestCode;
}

/** Revoke one of the caller's guest codes. Idempotent. */
export async function revokeGuestCode(id: number): Promise<void> {
  const res = await apiFetch(`/v1/guest_codes/${id}`, { method: "DELETE" });
  if (!res.ok && res.status !== 404) throw new Error(`HTTP ${res.status}`);
}

export { AuthError };
