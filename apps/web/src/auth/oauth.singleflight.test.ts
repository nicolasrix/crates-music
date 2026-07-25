// refreshTokens() coalesces concurrent callers onto one rotation (sec
// review 1.7). Without this, several API modules hitting a 401 at once
// each rotate the same refresh token; all but the winner get logged out.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

// The vitest `node` env has no localStorage; writeTokens/clearTokens use
// it. Minimal in-memory shim (get/set/remove/clear).
class MemStorage {
  private m = new Map<string, string>();
  get length(): number {
    return this.m.size;
  }
  key(i: number): string | null {
    return [...this.m.keys()][i] ?? null;
  }
  getItem(k: string): string | null {
    return this.m.has(k) ? this.m.get(k)! : null;
  }
  setItem(k: string, v: string): void {
    this.m.set(k, String(v));
  }
  removeItem(k: string): void {
    this.m.delete(k);
  }
  clear(): void {
    this.m.clear();
  }
}
globalThis.localStorage = new MemStorage() as unknown as Storage;
// oauth.ts reads location.origin at import time; provide a minimal stub.
globalThis.location = { origin: "http://localhost" } as unknown as Location;

// Dynamic import so the two globals above are in place before oauth.ts's
// module-level code runs (static ESM imports hoist above them).
let refreshTokens: (t: string) => Promise<void>;
beforeAll(async () => {
  ({ refreshTokens } = await import("./oauth"));
});

function okResponse(): Response {
  return {
    ok: true,
    json: async () => ({ access_token: "a2", refresh_token: "r2", expires_in: 3600 }),
  } as unknown as Response;
}

describe("refreshTokens singleflight (sec 1.7)", () => {
  beforeEach(() => {
    localStorage.clear();
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("coalesces concurrent refreshes into a single token request", async () => {
    let calls = 0;
    let release!: (r: Response) => void;
    const gated = new Promise<Response>((r) => {
      release = r;
    });
    const fetchMock = vi.fn(() => {
      calls += 1;
      return gated;
    });
    vi.stubGlobal("fetch", fetchMock);

    // Three concurrent callers, all holding the same old refresh token.
    const all = Promise.all([refreshTokens("r1"), refreshTokens("r1"), refreshTokens("r1")]);
    release(okResponse());
    await all;

    expect(calls).toBe(1); // exactly one network rotation
    expect(localStorage.getItem("gw_access_token")).toBe("a2");
    expect(localStorage.getItem("gw_refresh_token")).toBe("r2");
  });

  it("permits a new refresh after the in-flight one settles", async () => {
    const fetchMock = vi.fn(async () => okResponse());
    vi.stubGlobal("fetch", fetchMock);

    await refreshTokens("r1");
    await refreshTokens("r2");

    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("propagates failure to all coalesced callers and clears the guard", async () => {
    const failMock = vi.fn(async () => ({ ok: false, status: 400 }) as unknown as Response);
    vi.stubGlobal("fetch", failMock);

    const results = await Promise.allSettled([refreshTokens("r1"), refreshTokens("r1")]);
    expect(results.every((r) => r.status === "rejected")).toBe(true);
    expect(failMock).toHaveBeenCalledTimes(1);
    // Tokens were cleared on failure.
    expect(localStorage.getItem("gw_access_token")).toBeNull();

    // Guard reset: a subsequent refresh issues a fresh request.
    vi.stubGlobal("fetch", vi.fn(async () => okResponse()));
    await refreshTokens("r3");
    expect(localStorage.getItem("gw_access_token")).toBe("a2");
  });
});
