import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// `auth/oauth` reads `location.origin` at module load; stub it before the
// static import chain pulls it in (the `node` test env has no location).
vi.mock("../auth/oauth", () => ({ refreshTokens: vi.fn() }));
// `./client` (getSong) isn't exercised here and drags in image/url helpers.
vi.mock("./client", () => ({ getSong: vi.fn() }));

const store = new Map<string, string>();
vi.stubGlobal("localStorage", {
  getItem: (k: string) => store.get(k) ?? null,
  setItem: (k: string, v: string) => void store.set(k, v),
  removeItem: (k: string) => void store.delete(k),
  clear: () => store.clear(),
});

import { addTracksToPlaylist, removeTrackFromPlaylist } from "./playlists";

const ACCESS = "tok-access";
function seedTokens(): void {
  localStorage.setItem("gw_access_token", ACCESS);
  localStorage.setItem("gw_refresh_token", "tok-refresh");
  localStorage.setItem("gw_access_expires_at", String(Date.now() + 60_000));
}

describe("removeTrackFromPlaylist", () => {
  beforeEach(() => localStorage.clear());
  afterEach(() => vi.restoreAllMocks());

  it("replaces membership with the raw ids minus the target, preserving unhydrated ids", async () => {
    seedTokens();
    const fetchSpy = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchSpy);

    // "gone" is a stored id that failed to hydrate this load — it must NOT
    // be lost when we remove an unrelated track.
    const raw = ["a", "gone", "b", "c"];
    const next = await removeTrackFromPlaylist("pl1", "b", raw);

    expect(next).toEqual(["a", "gone", "c"]);
    const [url, init] = fetchSpy.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/v1/playlists/pl1/tracks");
    expect(init.method).toBe("PUT");
    const body = JSON.parse(init.body as string) as Record<string, unknown>;
    expect(body).toEqual({ track_ids: ["a", "gone", "c"], mode: "replace" });
  });

  it("removes every occurrence of a duplicated id", async () => {
    seedTokens();
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(null, { status: 204 })));
    const next = await removeTrackFromPlaylist("pl1", "d", ["d", "e", "d", "f"]);
    expect(next).toEqual(["e", "f"]);
  });

  it("encodes the playlist id in the path", async () => {
    seedTokens();
    const fetchSpy = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchSpy);
    await removeTrackFromPlaylist("pl/weird id", "x", ["x", "y"]);
    const [url] = fetchSpy.mock.calls[0] as [string];
    expect(url).toBe("/v1/playlists/pl%2Fweird%20id/tracks");
  });
});

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

describe("addTracksToPlaylist", () => {
  beforeEach(() => localStorage.clear());
  afterEach(() => vi.restoreAllMocks());

  it("appends and returns the gateway's added/skipped split", async () => {
    seedTokens();
    const fetchSpy = vi
      .fn()
      .mockResolvedValue(jsonResponse({ added: 2, skipped: 1 }));
    vi.stubGlobal("fetch", fetchSpy);

    const result = await addTracksToPlaylist("pl1", ["a", "b", "c"]);

    expect(result).toEqual({ added: 2, skipped: 1 });
    const [, init] = fetchSpy.mock.calls[0] as [string, RequestInit];
    const body = JSON.parse(init.body as string) as Record<string, unknown>;
    expect(body).toEqual({ track_ids: ["a", "b", "c"], mode: "append" });
  });

  it("reports a fully-duplicate add as nothing added", async () => {
    seedTokens();
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(jsonResponse({ added: 0, skipped: 1 })));
    expect(await addTracksToPlaylist("pl1", ["a"])).toEqual({ added: 0, skipped: 1 });
  });

  // A gateway predating the counts answers 204 with no body; the write
  // still happened, so assume nothing was a duplicate rather than throw.
  it("falls back to added=n against a countless 204", async () => {
    seedTokens();
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(null, { status: 204 })));
    expect(await addTracksToPlaylist("pl1", ["a", "b"])).toEqual({ added: 2, skipped: 0 });
  });

  it("short-circuits an empty id list without a request", async () => {
    seedTokens();
    const fetchSpy = vi.fn();
    vi.stubGlobal("fetch", fetchSpy);
    expect(await addTracksToPlaylist("pl1", [])).toEqual({ added: 0, skipped: 0 });
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("throws on a failed write", async () => {
    seedTokens();
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(null, { status: 500 })));
    await expect(addTracksToPlaylist("pl1", ["a"])).rejects.toThrow("HTTP 500");
  });
});
