// Unit tests for the IndexedDB audio cache. Runs in vitest's `node` env
// with fake-indexeddb patching the global indexedDB. A monotonic injected
// clock makes LRU ordering deterministic.

import "fake-indexeddb/auto";
import { beforeEach, describe, expect, it } from "vitest";

import { AudioCache } from "./audioCache";
import { type AudioKey } from "./audioKey";
import { type CacheBudgets } from "./cacheSettings";

let dbSeq = 0;

function makeCache(budgets: CacheBudgets) {
  // Unique db per cache so tests don't share state.
  const dbName = `test-audio-${dbSeq++}`;
  let t = 0;
  const now = () => ++t;
  const live = { ...budgets };
  const cache = new AudioCache({ dbName, budgets: () => live, now });
  return { cache, live };
}

function key(trackId: string): AudioKey {
  return { trackId, bitrate: null, codec: "mp3" };
}

function blob(bytes: number): Blob {
  return new Blob([new Uint8Array(bytes)]);
}

describe("AudioCache", () => {
  beforeEach(() => {
    dbSeq += 1; // extra spacing between suites
  });

  it("stores and retrieves a blob by track id", async () => {
    const { cache } = makeCache({ regularBudgetBytes: 1000, pinnedBudgetBytes: 1000 });
    await cache.put(key("a"), blob(100));

    const meta = await cache.getMetaByTrack("a");
    expect(meta).not.toBeNull();
    expect(meta!.bytes).toBe(100);
    expect(meta!.pinned).toBe(false);

    const b = await cache.getBlob(meta!.key);
    expect(b).not.toBeNull();
    expect(b!.size).toBe(100);
  });

  it("LRU-evicts the oldest regular entry when over budget", async () => {
    // budget fits 2 of 100, not 3.
    const { cache } = makeCache({ regularBudgetBytes: 250, pinnedBudgetBytes: 1000 });
    await cache.put(key("a"), blob(100));
    await cache.put(key("b"), blob(100));
    await cache.put(key("c"), blob(100)); // triggers eviction of "a"

    expect(await cache.getMetaByTrack("a")).toBeNull();
    expect(await cache.getMetaByTrack("b")).not.toBeNull();
    expect(await cache.getMetaByTrack("c")).not.toBeNull();
  });

  it("never evicts the only remaining regular row even if it exceeds budget", async () => {
    const { cache } = makeCache({ regularBudgetBytes: 150, pinnedBudgetBytes: 1000 });
    await cache.put(key("big"), blob(500));
    expect(await cache.getMetaByTrack("big")).not.toBeNull();
  });

  it("never LRU-evicts a pinned entry", async () => {
    const { cache } = makeCache({ regularBudgetBytes: 150, pinnedBudgetBytes: 1000 });
    const pinned = await cache.put(key("p"), blob(100));
    expect((await cache.pin(pinned.key)).kind).toBe("pinned");

    // Flood the regular budget; the pinned entry must survive.
    await cache.put(key("a"), blob(100));
    await cache.put(key("b"), blob(100));
    await cache.put(key("c"), blob(100));

    const p = await cache.getMetaByTrack("p");
    expect(p).not.toBeNull();
    expect(p!.pinned).toBe(true);
  });

  it("refuses a pin that would exceed the pinned budget", async () => {
    const { cache } = makeCache({ regularBudgetBytes: 1000, pinnedBudgetBytes: 50 });
    const e = await cache.put(key("a"), blob(100));
    const outcome = await cache.pin(e.key);
    expect(outcome).toEqual({ kind: "would-exceed-budget", overBy: 50 });
    expect((await cache.getMetaByTrack("a"))!.pinned).toBe(false);
  });

  it("is idempotent on a double pin / unpin", async () => {
    const { cache } = makeCache({ regularBudgetBytes: 1000, pinnedBudgetBytes: 1000 });
    const e = await cache.put(key("a"), blob(100));
    expect((await cache.pin(e.key)).kind).toBe("pinned");
    expect((await cache.pin(e.key)).kind).toBe("already-pinned");
    expect((await cache.unpin(e.key)).kind).toBe("unpinned");
    expect((await cache.unpin(e.key)).kind).toBe("not-pinned");
  });

  it("reports not-in-cache for pin/unpin of an unknown key", async () => {
    const { cache } = makeCache({ regularBudgetBytes: 1000, pinnedBudgetBytes: 1000 });
    expect((await cache.pin("missing|orig|mp3")).kind).toBe("not-in-cache");
    expect((await cache.unpin("missing|orig|mp3")).kind).toBe("not-in-cache");
  });

  it("preserves the pinned flag on re-put", async () => {
    const { cache } = makeCache({ regularBudgetBytes: 1000, pinnedBudgetBytes: 1000 });
    const e = await cache.put(key("a"), blob(100));
    await cache.pin(e.key);
    await cache.put(key("a"), blob(120)); // re-fetch, larger

    const meta = await cache.getMetaByTrack("a");
    expect(meta!.pinned).toBe(true);
    expect(meta!.bytes).toBe(120);
  });

  it("evicts newly-unpinned bytes that overflow the regular budget", async () => {
    const { cache } = makeCache({ regularBudgetBytes: 150, pinnedBudgetBytes: 1000 });
    const x = await cache.put(key("x"), blob(200)); // alone, protected
    await cache.pin(x.key); // move out of regular budget
    await cache.put(key("y"), blob(100)); // regular, fits

    expect((await cache.unpin(x.key)).kind).toBe("unpinned");
    // x (older) re-enters regular; x+y = 300 > 150 → x evicted, y kept.
    expect(await cache.getMetaByTrack("x")).toBeNull();
    expect(await cache.getMetaByTrack("y")).not.toBeNull();
  });

  it("a touch protects an otherwise-oldest entry from eviction", async () => {
    const { cache } = makeCache({ regularBudgetBytes: 250, pinnedBudgetBytes: 1000 });
    const a = await cache.put(key("a"), blob(100));
    await cache.put(key("b"), blob(100));
    await cache.touch(a.key); // a is now newest
    await cache.put(key("c"), blob(100)); // evicts the LRU = b, not a

    expect(await cache.getMetaByTrack("a")).not.toBeNull();
    expect(await cache.getMetaByTrack("b")).toBeNull();
    expect(await cache.getMetaByTrack("c")).not.toBeNull();
  });

  it("evicts immediately when the regular budget is lowered", async () => {
    const { cache, live } = makeCache({ regularBudgetBytes: 500, pinnedBudgetBytes: 1000 });
    await cache.put(key("a"), blob(100));
    await cache.put(key("b"), blob(100));
    await cache.put(key("c"), blob(100)); // all fit under 500

    live.regularBudgetBytes = 150; // user lowers the cap
    await cache.evictLruToFit();

    // Only the newest survives (a,b evicted, ≥1 kept).
    expect(await cache.getMetaByTrack("a")).toBeNull();
    expect(await cache.getMetaByTrack("b")).toBeNull();
    expect(await cache.getMetaByTrack("c")).not.toBeNull();
  });

  it("computes split stats for regular vs pinned", async () => {
    const { cache } = makeCache({ regularBudgetBytes: 1000, pinnedBudgetBytes: 1000 });
    const a = await cache.put(key("a"), blob(100));
    await cache.put(key("b"), blob(50));
    await cache.pin(a.key);

    const s = await cache.stats();
    expect(s.pinnedCount).toBe(1);
    expect(s.pinnedBytes).toBe(100);
    expect(s.regularCount).toBe(1);
    expect(s.regularBytes).toBe(50);
    expect(s.regularBudgetBytes).toBe(1000);
  });

  it("notifies delete listeners with evicted track ids", async () => {
    const { cache } = makeCache({ regularBudgetBytes: 150, pinnedBudgetBytes: 1000 });
    const seen: string[] = [];
    const off = cache.onDelete((ids) => seen.push(...ids));

    await cache.put(key("a"), blob(100));
    await cache.put(key("b"), blob(100)); // evicts "a"

    expect(seen).toContain("a");
    off();
  });

  it("listPinned returns pinned entries sorted by track id", async () => {
    const { cache } = makeCache({ regularBudgetBytes: 1000, pinnedBudgetBytes: 1000 });
    const z = await cache.put(key("z"), blob(10));
    const m = await cache.put(key("m"), blob(10));
    await cache.put(key("r"), blob(10)); // left unpinned
    await cache.pin(z.key);
    await cache.pin(m.key);

    const pinned = await cache.listPinned();
    expect(pinned.map((e) => e.trackId)).toEqual(["m", "z"]);
  });

  it("hard-deletes a pinned entry", async () => {
    const { cache } = makeCache({ regularBudgetBytes: 1000, pinnedBudgetBytes: 1000 });
    const e = await cache.put(key("a"), blob(100));
    await cache.pin(e.key);
    expect(await cache.delete(e.key)).toBe(true);
    expect(await cache.getMetaByTrack("a")).toBeNull();
    expect(await cache.getBlob(e.key)).toBeNull();
  });
});
