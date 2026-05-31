import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// `auth/oauth` touches `location.origin` at module load; this test runs in
// the default `node` env where there is no `location`. Stub it before the
// static import chain pulls it in.
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

import { postEvents } from "./events";

const ACCESS = "tok-access";
const REFRESH = "tok-refresh";

function seedTokens(): void {
  localStorage.setItem("gw_access_token", ACCESS);
  localStorage.setItem("gw_refresh_token", REFRESH);
  localStorage.setItem("gw_access_expires_at", String(Date.now() + 60_000));
}

describe("postEvents", () => {
  beforeEach(() => {
    localStorage.clear();
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("POSTs the batch wrapped in { events } with the bearer token", async () => {
    seedTokens();
    const fetchSpy = vi
      .fn()
      .mockResolvedValue(new Response(JSON.stringify({ accepted: 1 }), { status: 202 }));
    vi.stubGlobal("fetch", fetchSpy);

    await postEvents([
      {
        event_type: "skip",
        track_id: "t0",
        occurred_at: 1_700_000_000_000,
        metadata: { played_ms: 4200 },
      },
    ]);

    expect(fetchSpy).toHaveBeenCalledTimes(1);
    const [url, init] = fetchSpy.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/v1/events");
    expect(init.method).toBe("POST");
    expect((init.headers as Record<string, string>).Authorization).toBe(`Bearer ${ACCESS}`);
    expect((init.headers as Record<string, string>)["Content-Type"]).toBe("application/json");
    const body = JSON.parse(init.body as string) as { events: unknown[] };
    expect(body.events).toHaveLength(1);
    expect(body.events[0]).toEqual({
      event_type: "skip",
      track_id: "t0",
      occurred_at: 1_700_000_000_000,
      metadata: { played_ms: 4200 },
    });
  });

  it("short-circuits an empty batch without touching the network", async () => {
    seedTokens();
    const fetchSpy = vi.fn();
    vi.stubGlobal("fetch", fetchSpy);

    await postEvents([]);

    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("throws on a non-2xx response so callers can observe failure", async () => {
    seedTokens();
    const fetchSpy = vi.fn().mockResolvedValue(new Response("nope", { status: 500 }));
    vi.stubGlobal("fetch", fetchSpy);

    await expect(
      postEvents([{ event_type: "skip", track_id: "t0", occurred_at: 1 }])
    ).rejects.toThrow(/events HTTP 500/);
  });
});
