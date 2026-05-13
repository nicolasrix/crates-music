import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// `auth/oauth` touches `location.origin` at module load; this test
// runs in the default `node` env, where there is no `location`.
// Stub it before the static import chain pulls it in.
vi.mock("../auth/oauth", () => ({
  refreshTokens: vi.fn(),
}));

const store = new Map<string, string>();
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

import { invalidateBrowseCache } from "./diagnostics";

const ACCESS = "tok-access";
const REFRESH = "tok-refresh";

function seedTokens(): void {
  localStorage.setItem("gw_access_token", ACCESS);
  localStorage.setItem("gw_refresh_token", REFRESH);
  localStorage.setItem("gw_access_expires_at", String(Date.now() + 60_000));
}

describe("invalidateBrowseCache", () => {
  beforeEach(() => {
    localStorage.clear();
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("POSTs to /v1/admin/cache/invalidate with the bearer token", async () => {
    seedTokens();
    const fetchSpy = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ removed: 7 }), { status: 200 }),
    );
    vi.stubGlobal("fetch", fetchSpy);

    const result = await invalidateBrowseCache();

    expect(result.removed).toBe(7);
    expect(fetchSpy).toHaveBeenCalledTimes(1);
    const [url, init] = fetchSpy.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/v1/admin/cache/invalidate");
    expect(init.method).toBe("POST");
    const headers = init.headers as Record<string, string>;
    expect(headers.Authorization).toBe(`Bearer ${ACCESS}`);
  });

  it("rejects when the user isn't signed in", async () => {
    const fetchSpy = vi.fn();
    vi.stubGlobal("fetch", fetchSpy);
    await expect(invalidateBrowseCache()).rejects.toThrow(/not signed in/);
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("surfaces a non-2xx response as a thrown error", async () => {
    seedTokens();
    const fetchSpy = vi.fn().mockResolvedValue(new Response("err", { status: 500 }));
    vi.stubGlobal("fetch", fetchSpy);
    await expect(invalidateBrowseCache()).rejects.toThrow(/HTTP 500/);
  });
});
