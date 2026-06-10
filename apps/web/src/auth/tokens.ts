// Token persistence. Single-user homelab → localStorage is acceptable
// (the entire trust boundary is "anyone with browser access to the
// laptop can play music"); for production multi-user we'd move the
// refresh token to an httpOnly cookie.

const ACCESS_KEY = "gw_access_token";
const ACCESS_EXPIRES_KEY = "gw_access_expires_at";
const REFRESH_KEY = "gw_refresh_token";
const VERIFIER_KEY = "gw_pkce_verifier";
const STATE_KEY = "gw_oauth_state";

export interface TokenPair {
  accessToken: string;
  // `null` for guest sessions: the guest grant (PR D) issues a single
  // long-lived access token bounded by the guest account's expiry, with no
  // refresh token. When it lapses the visitor re-redeems the code.
  refreshToken: string | null;
  expiresAt: number; // unix-ms
}

export function readTokens(): TokenPair | null {
  const access = localStorage.getItem(ACCESS_KEY);
  const refresh = localStorage.getItem(REFRESH_KEY);
  const expiresAt = localStorage.getItem(ACCESS_EXPIRES_KEY);
  if (!access || !expiresAt) return null;
  return { accessToken: access, refreshToken: refresh, expiresAt: Number(expiresAt) };
}

export function writeTokens(p: TokenPair) {
  localStorage.setItem(ACCESS_KEY, p.accessToken);
  if (p.refreshToken) {
    localStorage.setItem(REFRESH_KEY, p.refreshToken);
  } else {
    localStorage.removeItem(REFRESH_KEY);
  }
  localStorage.setItem(ACCESS_EXPIRES_KEY, String(p.expiresAt));
}

export function clearTokens() {
  localStorage.removeItem(ACCESS_KEY);
  localStorage.removeItem(REFRESH_KEY);
  localStorage.removeItem(ACCESS_EXPIRES_KEY);
}

// PKCE verifier lives in sessionStorage so it dies with the tab — it
// only needs to outlive the redirect to the authorize endpoint.

export function stashVerifier(v: string) {
  sessionStorage.setItem(VERIFIER_KEY, v);
}

export function popVerifier(): string | null {
  const v = sessionStorage.getItem(VERIFIER_KEY);
  if (v) sessionStorage.removeItem(VERIFIER_KEY);
  return v;
}

// The OAuth `state` value lives alongside the verifier (sessionStorage,
// tab-scoped). The callback compares the returned `state` against this
// to defend against login CSRF — an attacker-seeded callback won't carry
// the value this tab generated.

export function stashState(s: string) {
  sessionStorage.setItem(STATE_KEY, s);
}

export function popState(): string | null {
  const v = sessionStorage.getItem(STATE_KEY);
  if (v) sessionStorage.removeItem(STATE_KEY);
  return v;
}
