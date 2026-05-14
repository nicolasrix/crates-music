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

import {
  fetchSpanChildren,
  fetchSpanSeries,
  invalidateBrowseCache,
} from "./diagnostics";

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

describe("fetchSpanSeries", () => {
  beforeEach(() => {
    localStorage.clear();
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("encodes name, since_ms, and limit in the query string", async () => {
    seedTokens();
    const body = { name: "ingest.fetch_clip", points: [] };
    const fetchSpy = vi.fn().mockResolvedValue(
      new Response(JSON.stringify(body), { status: 200 }),
    );
    vi.stubGlobal("fetch", fetchSpy);

    const result = await fetchSpanSeries({
      name: "ingest.fetch_clip",
      sinceMs: 12345,
      limit: 500,
    });

    expect(result).toEqual(body);
    expect(fetchSpy).toHaveBeenCalledTimes(1);
    const [url] = fetchSpy.mock.calls[0] as [string, RequestInit];
    expect(url.startsWith("/v1/diagnostics/span_series?")).toBe(true);
    const qs = new URLSearchParams(url.split("?")[1]);
    expect(qs.get("name")).toBe("ingest.fetch_clip");
    expect(qs.get("since_ms")).toBe("12345");
    expect(qs.get("limit")).toBe("500");
  });

  it("omits optional params when not provided", async () => {
    seedTokens();
    const fetchSpy = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ name: "x", points: [] }), { status: 200 }),
    );
    vi.stubGlobal("fetch", fetchSpy);

    await fetchSpanSeries({ name: "x" });

    const [url] = fetchSpy.mock.calls[0] as [string, RequestInit];
    const qs = new URLSearchParams(url.split("?")[1]);
    expect(qs.get("name")).toBe("x");
    expect(qs.has("since_ms")).toBe(false);
    expect(qs.has("limit")).toBe(false);
  });

  it("parses points from the response body", async () => {
    seedTokens();
    const points = [
      { end_ms: 1000, duration_ms: 50 },
      { end_ms: 2000, duration_ms: 75 },
    ];
    const fetchSpy = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ name: "x", points }), { status: 200 }),
    );
    vi.stubGlobal("fetch", fetchSpy);

    const r = await fetchSpanSeries({ name: "x" });
    expect(r.points).toEqual(points);
  });
});

describe("fetchSpanChildren", () => {
  beforeEach(() => {
    localStorage.clear();
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("encodes name and optional since_ms", async () => {
    seedTokens();
    const fetchSpy = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          parent_name: "p",
          parent_count: 0,
          parent_sum_ms: 0,
          children: [],
        }),
        { status: 200 },
      ),
    );
    vi.stubGlobal("fetch", fetchSpy);

    await fetchSpanChildren({ name: "ingest.fetch_clip", sinceMs: 99 });
    const [url] = fetchSpy.mock.calls[0] as [string, RequestInit];
    expect(url.startsWith("/v1/diagnostics/span_children?")).toBe(true);
    const qs = new URLSearchParams(url.split("?")[1]);
    expect(qs.get("name")).toBe("ingest.fetch_clip");
    expect(qs.get("since_ms")).toBe("99");
  });

  it("parses children with mean_ms", async () => {
    seedTokens();
    const body = {
      parent_name: "p",
      parent_count: 2,
      parent_sum_ms: 3000,
      children: [
        { name: "c1", count: 2, sum_ms: 2500, mean_ms: 1250 },
        { name: "c2", count: 2, sum_ms: 30, mean_ms: 15 },
      ],
    };
    const fetchSpy = vi.fn().mockResolvedValue(
      new Response(JSON.stringify(body), { status: 200 }),
    );
    vi.stubGlobal("fetch", fetchSpy);

    const r = await fetchSpanChildren({ name: "p" });
    expect(r.children).toHaveLength(2);
    expect(r.children[0]!.mean_ms).toBe(1250);
    expect(r.parent_sum_ms).toBe(3000);
  });
});
