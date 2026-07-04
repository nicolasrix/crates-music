// OAuth 2.1 PKCE flow against the gateway's /oauth/* endpoints.
//
// The Vite dev server proxies /oauth/* to the gateway, so we use
// same-origin URLs throughout — that keeps redirect_uri and the
// initial-load origin in sync without per-environment URL config.

import { deriveChallenge, generateVerifier } from "./pkce";
import {
  clearTokens,
  popState,
  popVerifier,
  stashState,
  stashVerifier,
  writeTokens,
} from "./tokens";

const CLIENT_ID = "web";
const REDIRECT_URI = `${location.origin}/oauth/callback`;

export async function startLogin(options?: { forceLogin?: boolean }) {
  const verifier = await generateVerifier();
  const challenge = await deriveChallenge(verifier);
  const state = crypto.randomUUID();
  stashVerifier(verifier);
  stashState(state);

  const params = new URLSearchParams({
    response_type: "code",
    client_id: CLIENT_ID,
    redirect_uri: REDIRECT_URI,
    code_challenge: challenge,
    code_challenge_method: "S256",
    state,
  });
  // `forceLogin` → prompt=login: the gateway ignores any lingering
  // gw_session cookie and shows the login screen, so the user can sign in
  // as a different account instead of being silently re-authed. Without it,
  // an existing session is reused (one-click resume).
  if (options?.forceLogin) params.set("prompt", "login");
  // Full-page redirect — the gateway's authorize handler will bounce
  // through /oauth/login if no session, then back to redirect_uri.
  location.assign(`/oauth/authorize?${params.toString()}`);
}

export async function completeLogin(code: string, returnedState: string | null): Promise<void> {
  // Pop both before any early return so a failed attempt can't be
  // replayed against a stale verifier/state left in sessionStorage.
  const expectedState = popState();
  const verifier = popVerifier();
  if (!expectedState || returnedState !== expectedState) {
    throw new Error("OAuth state mismatch — aborting sign-in (possible CSRF)");
  }
  if (!verifier) {
    throw new Error("missing PKCE verifier — did you reload the callback page?");
  }
  const body = new URLSearchParams({
    grant_type: "authorization_code",
    code,
    client_id: CLIENT_ID,
    redirect_uri: REDIRECT_URI,
    code_verifier: verifier,
  });
  const res = await fetch("/oauth/token", {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: body.toString(),
  });
  if (!res.ok) {
    throw new Error(`token exchange failed: HTTP ${res.status}`);
  }
  const json = (await res.json()) as {
    access_token: string;
    refresh_token: string;
    expires_in: number;
  };
  writeTokens({
    accessToken: json.access_token,
    refreshToken: json.refresh_token,
    expiresAt: Date.now() + json.expires_in * 1000,
  });
}

// Redeem a guest code (PR D). No PKCE, no redirect — the code is the
// credential. The gateway returns a single access token (no refresh) bound
// to an ephemeral guest principal that shares the host's room. Returns the
// host's user id (the room the guest joined).
export async function joinAsGuest(
  code: string,
  displayName?: string,
): Promise<{ hostUserId: number }> {
  const body = new URLSearchParams({
    code: code.trim(),
    client_id: CLIENT_ID,
  });
  if (displayName && displayName.trim()) {
    body.set("display_name", displayName.trim());
  }
  const res = await fetch("/oauth/guest", {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: body.toString(),
  });
  if (!res.ok) {
    // Surface the gateway's reason ("this guest code is no longer valid",
    // "unknown guest code") so the join form can show it.
    let message = `HTTP ${res.status}`;
    try {
      const err = (await res.json()) as { error_description?: string };
      if (err.error_description) message = err.error_description;
    } catch {
      // non-JSON body; keep the status message
    }
    throw new Error(message);
  }
  const json = (await res.json()) as {
    access_token: string;
    expires_in: number;
    host_user_id: number;
  };
  writeTokens({
    accessToken: json.access_token,
    refreshToken: null,
    expiresAt: Date.now() + json.expires_in * 1000,
  });
  return { hostUserId: json.host_user_id };
}

/** The three distinguishable results of a refresh attempt.
 *
 * The critical distinction is `rejected` vs `unavailable`: only a
 * definitive server rejection proves the session is dead. A network
 * failure means we simply couldn't ask — throwing the credentials away
 * for that would sign a mobile user out the moment they step off the
 * home network (the gateway becomes unreachable), which is exactly the
 * behaviour we're fixing. See `docs/plans/auth-offline-resilience.md`. */
export type RefreshOutcome =
  /** New token pair written to storage. */
  | "ok"
  /** The gateway positively rejected the refresh token (400/401
   *  `invalid_grant`) — revoked, rotated away, or unknown. The session is
   *  genuinely over; tokens are cleared. */
  | "rejected"
  /** We couldn't reach a healthy gateway — `fetch` rejected (offline,
   *  DNS/TLS failure) or the server answered 5xx (up-but-unhealthy /
   *  restarting / behind a proxy error). Indeterminate: tokens are
   *  **kept** so an installed PWA stays usable offline and can retry once
   *  connectivity returns. */
  | "unavailable";

export async function refreshTokens(refreshToken: string): Promise<RefreshOutcome> {
  const body = new URLSearchParams({
    grant_type: "refresh_token",
    client_id: CLIENT_ID,
    refresh_token: refreshToken,
  });
  let res: Response;
  try {
    res = await fetch("/oauth/token", {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: body.toString(),
    });
  } catch {
    // The gateway never answered — offline, DNS, TLS, connection reset.
    // We cannot conclude the refresh token is bad, so keep it.
    return "unavailable";
  }
  if (res.ok) {
    const json = (await res.json()) as {
      access_token: string;
      refresh_token: string;
      expires_in: number;
    };
    writeTokens({
      accessToken: json.access_token,
      refreshToken: json.refresh_token,
      expiresAt: Date.now() + json.expires_in * 1000,
    });
    return "ok";
  }
  // A 400/401 from the token endpoint is the OAuth server telling us the
  // grant is invalid (RFC 6749 §5.2 `invalid_grant`). That is the only
  // signal that definitively ends the session — clear the tokens so the
  // app falls back to the sign-in screen.
  if (res.status === 400 || res.status === 401) {
    clearTokens();
    return "rejected";
  }
  // 5xx and anything else: the endpoint is reachable but unhealthy
  // (restart, proxy 502/503/504). Transient — keep the tokens and let the
  // caller retry on the next tick / reconnect.
  return "unavailable";
}

export async function logout(refreshToken: string | null) {
  if (refreshToken) {
    try {
      await fetch("/oauth/revoke", {
        method: "POST",
        headers: { "Content-Type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams({ token: refreshToken }).toString(),
      });
    } catch {
      // best-effort; clear local state regardless
    }
  }
  clearTokens();
}
