// Wrapper for the gateway's gateway-owned per-track like/dislike:
//   PUT /v1/library/rating   — set or clear one track's verdict
//   GET /v1/library/ratings  — every rated track (ids + verdict)
//
// This is *our* taste store, deliberately not Subsonic star/unstar — the
// gateway never writes back to Navidrome. Distinct from the recommendation
// thumbs (api/recommend.ts): this rates the song itself and is durable.
//
// Auth piggybacks on the same Bearer-with-refresh dance as events.ts /
// recommend.ts. Copied locally for the same reason noted there (client.ts's
// apiFetch is GET-only and Subsonic-envelope-focused).

import { refreshTokens } from "../auth/oauth";
import { clearTokens, readTokens } from "../auth/tokens";

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

/** A durable per-track verdict. `null` is neutral (no rating). */
export type Rating = "like" | "dislike" | null;

interface RatingRow {
  track_id: string;
  rating: "like" | "dislike";
}

/** Fetch every rated track. Ids + verdict only; the caller hydrates
 *  titles/art client-side (the gateway's metadata lacks cover art). */
export async function getRatings(): Promise<RatingRow[]> {
  const res = await apiFetch("/v1/library/ratings");
  if (!res.ok) throw new Error(`ratings HTTP ${res.status}`);
  const body = (await res.json()) as { ratings: RatingRow[] };
  return body.ratings;
}

/** Set (`"like"`/`"dislike"`) or clear (`null`) a track's rating. */
export async function putRating(trackId: string, rating: Rating): Promise<void> {
  const res = await apiFetch("/v1/library/rating", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ track_id: trackId, rating }),
  });
  if (!res.ok) throw new Error(`rating HTTP ${res.status}`);
}

export { AuthError };
