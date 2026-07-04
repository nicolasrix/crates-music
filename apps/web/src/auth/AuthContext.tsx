import { createContext, ReactNode, useContext, useEffect, useState } from "react";
import { readTokens, TokenPair } from "./tokens";
import { joinAsGuest, refreshTokens, startLogin, logout as doLogout } from "./oauth";

interface AuthState {
  tokens: TokenPair | null;
  loading: boolean;
  /** Begin the PKCE sign-in. `switchUser` forces the gateway login screen
   *  (prompt=login) so a lingering session isn't silently reused. */
  login: (options?: { switchUser?: boolean }) => void;
  /** Redeem a guest code and enter the host's room (PR D). */
  joinGuest: (code: string, displayName?: string) => Promise<void>;
  logout: () => Promise<void>;
}

const Ctx = createContext<AuthState | null>(null);

export function AuthProvider({ children }: { children: ReactNode }) {
  const [tokens, setTokens] = useState<TokenPair | null>(readTokens());
  const [loading, setLoading] = useState(false);

  // On mount, if the stored access token is expired (or close to it),
  // try to refresh silently. Skipped on /oauth/callback where the
  // callback page handles the exchange itself.
  useEffect(() => {
    if (location.pathname === "/oauth/callback") return;
    const t = readTokens();
    if (!t) return;
    const skewMs = 30_000;
    if (t.expiresAt > Date.now() + skewMs) {
      setTokens(t);
      return;
    }
    // A lapsed guest session has no refresh path — drop it so the app
    // returns to the sign-in / join screen.
    if (!t.refreshToken) {
      setTokens(null);
      return;
    }
    setLoading(true);
    refreshTokens(t.refreshToken)
      .then((outcome) => {
        if (outcome === "rejected") {
          // The gateway positively rejected the refresh token — a real
          // sign-out. Fall back to the login screen.
          setTokens(null);
        } else {
          // "ok"          → fresh tokens in storage.
          // "unavailable" → offline / gateway unreachable. Keep the
          //   (expired) tokens so the installed PWA still boots into
          //   offline mode and can play pinned tracks; the reconnect
          //   effect below (or an API 401→refresh) re-establishes a live
          //   session once the gateway is reachable again.
          setTokens(readTokens() ?? t);
        }
      })
      .finally(() => setLoading(false));
  }, []);

  // When connectivity returns (wifi/cellular handoff, back on the home
  // network), silently re-establish a live session if the access token
  // has lapsed. Without this, a user who was kept signed-in offline would
  // keep hitting 401→refresh on every request until the first one
  // happens to land; refreshing eagerly on `online` makes recovery
  // immediate. A `rejected` here still means a genuine sign-out.
  useEffect(() => {
    function onOnline() {
      const t = readTokens();
      if (!t?.refreshToken) return;
      if (t.expiresAt > Date.now() + 30_000) return; // still valid
      void refreshTokens(t.refreshToken).then((outcome) => {
        if (outcome === "rejected") setTokens(null);
        else setTokens(readTokens() ?? t);
      });
    }
    window.addEventListener("online", onOnline);
    return () => window.removeEventListener("online", onOnline);
  }, []);

  const value: AuthState = {
    tokens,
    loading,
    login: (options?: { switchUser?: boolean }) => {
      void startLogin({ forceLogin: options?.switchUser ?? false });
    },
    joinGuest: async (code: string, displayName?: string) => {
      await joinAsGuest(code, displayName);
      setTokens(readTokens());
    },
    logout: async () => {
      await doLogout(tokens?.refreshToken ?? null);
      setTokens(null);
    },
  };
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useAuth(): AuthState {
  const v = useContext(Ctx);
  if (!v) throw new Error("useAuth must be used inside <AuthProvider>");
  return v;
}
