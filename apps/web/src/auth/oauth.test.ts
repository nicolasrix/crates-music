import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

// `auth/oauth` reads `location.origin` at module load (PKCE redirect_uri)
// and touches `localStorage` on write/clear. The default `node` env has
// neither. Stub both at top-level, then pull the module in via a *dynamic*
// import inside beforeAll — a static import is hoisted above these stubs
// and would blow up on `location.origin`.
const store = new Map<string, string>();
vi.stubGlobal("location", { origin: "https://gateway.local:8443", pathname: "/" });
vi.stubGlobal("localStorage", {
  getItem: (k: string) => store.get(k) ?? null,
  setItem: (k: string, v: string) => {
    store.set(k, v);
  },
  removeItem: (k: string) => {
    store.delete(k);
  },
  clear: () => {
    store.clear();
  },
});

let refreshTokens: (refreshToken: string) => Promise<"ok" | "rejected" | "unavailable">;
beforeAll(async () => {
  ({ refreshTokens } = await import("./oauth"));
});

const ACCESS_KEY = "gw_access_token";
const REFRESH_KEY = "gw_refresh_token";
const EXPIRES_KEY = "gw_access_expires_at";

function seedTokens(): void {
  localStorage.setItem(ACCESS_KEY, "old-access");
  localStorage.setItem(REFRESH_KEY, "old-refresh");
  localStorage.setItem(EXPIRES_KEY, String(Date.now() - 1000)); // expired
}

function tokensPresent(): boolean {
  return localStorage.getItem(REFRESH_KEY) !== null;
}

describe("refreshTokens outcomes", () => {
  beforeEach(() => {
    localStorage.clear();
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("returns 'ok' and writes the rotated pair on 200", async () => {
    seedTokens();
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(
          JSON.stringify({
            access_token: "new-access",
            refresh_token: "new-refresh",
            expires_in: 3600,
          }),
          { status: 200 },
        ),
      ),
    );

    const outcome = await refreshTokens("old-refresh");

    expect(outcome).toBe("ok");
    expect(localStorage.getItem(ACCESS_KEY)).toBe("new-access");
    expect(localStorage.getItem(REFRESH_KEY)).toBe("new-refresh");
  });

  // A definitive rejection is the ONLY thing that ends the session.
  it.each([400, 401])(
    "returns 'rejected' and clears tokens on HTTP %i (invalid_grant)",
    async (status) => {
      seedTokens();
      vi.stubGlobal(
        "fetch",
        vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: "invalid_grant" }), { status })),
      );

      const outcome = await refreshTokens("old-refresh");

      expect(outcome).toBe("rejected");
      expect(tokensPresent()).toBe(false);
    },
  );

  // The regression this whole change exists to prevent: an unreachable
  // gateway (off the home network) must NOT sign the user out.
  it("returns 'unavailable' and KEEPS tokens when fetch rejects (offline)", async () => {
    seedTokens();
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("Failed to fetch")));

    const outcome = await refreshTokens("old-refresh");

    expect(outcome).toBe("unavailable");
    expect(tokensPresent()).toBe(true);
    expect(localStorage.getItem(REFRESH_KEY)).toBe("old-refresh");
  });

  // A 5xx means the endpoint is reachable but unhealthy (restart / proxy
  // 502-504) — transient, so treat it like offline, not a sign-out.
  it.each([500, 502, 503, 504])(
    "returns 'unavailable' and KEEPS tokens on HTTP %i",
    async (status) => {
      seedTokens();
      vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("upstream error", { status })));

      const outcome = await refreshTokens("old-refresh");

      expect(outcome).toBe("unavailable");
      expect(tokensPresent()).toBe(true);
    },
  );
});
