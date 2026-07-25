import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// `auth/oauth` touches `location.origin` at module load; stub it before the
// static import chain pulls it in (default `node` test env has no location).
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

import { getRatings, putRating } from "./library";

const ACCESS = "tok-access";

function seedTokens(): void {
  localStorage.setItem("gw_access_token", ACCESS);
  localStorage.setItem("gw_refresh_token", "tok-refresh");
  localStorage.setItem("gw_access_expires_at", String(Date.now() + 60_000));
}

describe("putRating", () => {
  beforeEach(() => localStorage.clear());
  afterEach(() => vi.restoreAllMocks());

  it("PUTs {kind, id, rating} with the bearer token", async () => {
    seedTokens();
    const fetchSpy = vi.fn().mockResolvedValue(new Response("{}", { status: 200 }));
    vi.stubGlobal("fetch", fetchSpy);

    await putRating("track", "t0", "dislike");

    expect(fetchSpy).toHaveBeenCalledTimes(1);
    const [url, init] = fetchSpy.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/v1/library/rating");
    expect(init.method).toBe("PUT");
    expect((init.headers as Record<string, string>).Authorization).toBe(
      `Bearer ${ACCESS}`,
    );
    const body = JSON.parse(init.body as string) as Record<string, unknown>;
    expect(body).toEqual({ kind: "track", id: "t0", rating: "dislike" });
  });

  it("carries the kind for album / artist ratings", async () => {
    seedTokens();
    const fetchSpy = vi.fn().mockResolvedValue(new Response("{}", { status: 200 }));
    vi.stubGlobal("fetch", fetchSpy);

    await putRating("album", "al0", "like");
    await putRating("artist", "ar0", "dislike");

    const bodies = fetchSpy.mock.calls.map(
      (c) => JSON.parse((c[1] as RequestInit).body as string) as Record<string, unknown>,
    );
    expect(bodies[0]).toEqual({ kind: "album", id: "al0", rating: "like" });
    expect(bodies[1]).toEqual({ kind: "artist", id: "ar0", rating: "dislike" });
  });

  it("sends rating: null to clear", async () => {
    seedTokens();
    const fetchSpy = vi.fn().mockResolvedValue(new Response("{}", { status: 200 }));
    vi.stubGlobal("fetch", fetchSpy);

    await putRating("track", "t0", null);

    const [, init] = fetchSpy.mock.calls[0] as [string, RequestInit];
    const body = JSON.parse(init.body as string) as Record<string, unknown>;
    expect(body).toEqual({ kind: "track", id: "t0", rating: null });
  });

  it("throws on a non-2xx response", async () => {
    seedTokens();
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("no", { status: 500 })));
    await expect(putRating("track", "t0", "like")).rejects.toThrow(/rating HTTP 500/);
  });
});

describe("getRatings", () => {
  beforeEach(() => localStorage.clear());
  afterEach(() => vi.restoreAllMocks());

  it("GETs and unwraps the { ratings } envelope", async () => {
    seedTokens();
    const rows = [
      { kind: "track", id: "t1", rating: "like" },
      { kind: "album", id: "al2", rating: "dislike" },
      { kind: "artist", id: "ar3", rating: "like" },
    ];
    const fetchSpy = vi
      .fn()
      .mockResolvedValue(new Response(JSON.stringify({ ratings: rows }), { status: 200 }));
    vi.stubGlobal("fetch", fetchSpy);

    const got = await getRatings();

    expect(fetchSpy.mock.calls[0]![0]).toBe("/v1/library/ratings");
    expect(got).toEqual(rows);
  });

  it("throws on a non-2xx response", async () => {
    seedTokens();
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("no", { status: 503 })));
    await expect(getRatings()).rejects.toThrow(/ratings HTTP 503/);
  });
});
