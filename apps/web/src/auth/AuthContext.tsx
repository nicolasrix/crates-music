import { createContext, ReactNode, useContext, useEffect, useState } from "react";
import { readTokens, TokenPair } from "./tokens";
import { joinAsGuest, refreshTokens, startLogin, logout as doLogout } from "./oauth";

interface AuthState {
  tokens: TokenPair | null;
  loading: boolean;
  login: () => void;
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
      .then(() => setTokens(readTokens()))
      .catch(() => setTokens(null))
      .finally(() => setLoading(false));
  }, []);

  const value: AuthState = {
    tokens,
    loading,
    login: () => {
      void startLogin();
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
