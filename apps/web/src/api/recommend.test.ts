import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// `auth/oauth` touches `location.origin` at module load; this test
// runs in the default `node` env, where there is no `location`.
// Stub it before the static import chain pulls it in.
vi.mock("../auth/oauth", () => ({
  refreshTokens: vi.fn(),
}));
// `getSong` from ./client is unused by the new fetchers but still imported
// at module load; stub it so we don't hit the real Subsonic envelope code.
vi.mock("./client", () => ({
  getSong: vi.fn(),
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

import { fetchSimilarAlbums, fetchSimilarArtists } from "./recommend";

const ACCESS = "tok-access";
const REFRESH = "tok-refresh";

function seedTokens(): void {
  localStorage.setItem("gw_access_token", ACCESS);
  localStorage.setItem("gw_refresh_token", REFRESH);
  localStorage.setItem("gw_access_expires_at", String(Date.now() + 60_000));
}

describe("fetchSimilarAlbums", () => {
  beforeEach(() => {
    localStorage.clear();
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("POSTs seed_track_ids and exclude_album_ids in the body", async () => {
    seedTokens();
    const fetchSpy = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          model_version: "v1",
          results: [
            { album_id: "alb_b", score: 1.5, supporting_tracks: 2 },
            { album_id: "alb_c", score: 0.7, supporting_tracks: 1 },
          ],
          all_seeds_unindexed: false,
        }),
        { status: 200 },
      ),
    );
    vi.stubGlobal("fetch", fetchSpy);

    const r = await fetchSimilarAlbums({
      seedTrackIds: ["t0", "t1"],
      excludeAlbumIds: ["alb_a"],
      n: 5,
    });

    expect(r.results).toHaveLength(2);
    expect(r.results[0]!.album_id).toBe("alb_b");
    expect(r.results[0]!.supporting_tracks).toBe(2);
    expect(r.all_seeds_unindexed).toBe(false);

    expect(fetchSpy).toHaveBeenCalledTimes(1);
    const [url, init] = fetchSpy.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/v1/recommend/similar_albums");
    expect(init.method).toBe("POST");
    const body = JSON.parse(init.body as string) as Record<string, unknown>;
    expect(body.seed_track_ids).toEqual(["t0", "t1"]);
    expect(body.exclude_album_ids).toEqual(["alb_a"]);
    expect(body.n).toBe(5);
  });

  it("omits exclude_album_ids and n when not given", async () => {
    seedTokens();
    const fetchSpy = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({ model_version: null, results: [], all_seeds_unindexed: false }),
        { status: 200 },
      ),
    );
    vi.stubGlobal("fetch", fetchSpy);

    await fetchSimilarAlbums({ seedTrackIds: ["t0"] });

    const [, init] = fetchSpy.mock.calls[0] as [string, RequestInit];
    const body = JSON.parse(init.body as string) as Record<string, unknown>;
    expect(body.seed_track_ids).toEqual(["t0"]);
    expect("exclude_album_ids" in body).toBe(false);
    expect("n" in body).toBe(false);
  });

  it("surfaces all_seeds_unindexed=true", async () => {
    seedTokens();
    const fetchSpy = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({ model_version: "v1", results: [], all_seeds_unindexed: true }),
        { status: 200 },
      ),
    );
    vi.stubGlobal("fetch", fetchSpy);

    const r = await fetchSimilarAlbums({ seedTrackIds: ["unknown"] });
    expect(r.all_seeds_unindexed).toBe(true);
    expect(r.results).toEqual([]);
  });

  it("throws on non-2xx", async () => {
    seedTokens();
    const fetchSpy = vi.fn().mockResolvedValue(new Response("nope", { status: 500 }));
    vi.stubGlobal("fetch", fetchSpy);
    await expect(fetchSimilarAlbums({ seedTrackIds: ["t0"] })).rejects.toThrow(
      /HTTP 500/,
    );
  });
});

describe("fetchSimilarArtists", () => {
  beforeEach(() => {
    localStorage.clear();
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("POSTs seed_track_ids and exclude_artist_ids in the body", async () => {
    seedTokens();
    const fetchSpy = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          model_version: "v1",
          results: [{ artist_id: "ar_y", score: 2.3, supporting_tracks: 4 }],
          all_seeds_unindexed: false,
        }),
        { status: 200 },
      ),
    );
    vi.stubGlobal("fetch", fetchSpy);

    const r = await fetchSimilarArtists({
      seedTrackIds: ["t0", "t1"],
      excludeArtistIds: ["ar_x"],
      n: 8,
    });

    expect(r.results[0]!.artist_id).toBe("ar_y");
    expect(r.results[0]!.supporting_tracks).toBe(4);

    const [url, init] = fetchSpy.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/v1/recommend/similar_artists");
    const body = JSON.parse(init.body as string) as Record<string, unknown>;
    expect(body.seed_track_ids).toEqual(["t0", "t1"]);
    expect(body.exclude_artist_ids).toEqual(["ar_x"]);
    expect(body.n).toBe(8);
  });

  it("surfaces all_seeds_unindexed=true", async () => {
    seedTokens();
    const fetchSpy = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({ model_version: "v1", results: [], all_seeds_unindexed: true }),
        { status: 200 },
      ),
    );
    vi.stubGlobal("fetch", fetchSpy);

    const r = await fetchSimilarArtists({ seedTrackIds: ["unknown"] });
    expect(r.all_seeds_unindexed).toBe(true);
  });
});
