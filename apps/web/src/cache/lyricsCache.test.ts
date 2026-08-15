// Unit tests for the offline lyrics store. Same shape as audioCache.test.ts:
// vitest's `node` env with fake-indexeddb patching global indexedDB, and a
// unique database per test so nothing leaks between them.

import "fake-indexeddb/auto";
import { beforeEach, describe, expect, it } from "vitest";

import type { LyricsDoc } from "../api/lyrics";
import { LyricsCache } from "./lyricsCache";

let dbSeq = 0;

function makeCache(): LyricsCache {
  dbSeq += 1;
  return new LyricsCache({ dbName: `lyrics-test-${dbSeq}`, now: () => 1_000 });
}

function doc(trackId: string, text = "first line"): LyricsDoc {
  return {
    track_id: trackId,
    source: "lrclib",
    match_kind: "exact",
    synced: true,
    instrumental: false,
    lines: [{ start_ms: 0, text }],
    plain: null,
    provider_id: "42",
    fetched_at: 1,
  };
}

describe("LyricsCache", () => {
  let cache: LyricsCache;

  beforeEach(() => {
    cache = makeCache();
  });

  it("returns null for a track it has never seen", async () => {
    expect(await cache.get("nope")).toBeNull();
  });

  it("round-trips a document", async () => {
    await cache.put(doc("t1"));
    expect(await cache.get("t1")).toEqual(doc("t1"));
  });

  it("keys on the document's own track_id", async () => {
    // The store derives its key from the payload, so a caller cannot file a
    // document under the wrong track by passing a mismatched id.
    await cache.put(doc("t1"));
    await cache.put(doc("t2"));
    expect(await cache.count()).toBe(2);
  });

  it("put overwrites an existing copy", async () => {
    await cache.put(doc("t1", "old"));
    await cache.put(doc("t1", "new"));
    expect(await cache.count()).toBe(1);
    expect((await cache.get("t1"))?.lines?.[0]?.text).toBe("new");
  });

  describe("refreshIfPresent", () => {
    it("updates a stored copy and reports it did", async () => {
      await cache.put(doc("t1", "old"));
      expect(await cache.refreshIfPresent(doc("t1", "new"))).toBe(true);
      expect((await cache.get("t1"))?.lines?.[0]?.text).toBe("new");
    });

    it("does not create a row for a track that was never downloaded", async () => {
      // This is the property that bounds the store: browsing lyrics for a
      // thousand tracks must not persist a thousand rows.
      expect(await cache.refreshIfPresent(doc("t1"))).toBe(false);
      expect(await cache.count()).toBe(0);
    });
  });

  it("deletes a single track", async () => {
    await cache.put(doc("t1"));
    await cache.delete("t1");
    expect(await cache.get("t1")).toBeNull();
  });

  it("deleting an absent track is a no-op, not an error", async () => {
    await expect(cache.delete("ghost")).resolves.toBeUndefined();
  });

  it("deleteMany drops every listed track and ignores the rest", async () => {
    await cache.put(doc("t1"));
    await cache.put(doc("t2"));
    await cache.put(doc("t3"));
    await cache.deleteMany(["t1", "t3", "never-stored"]);
    expect(await cache.count()).toBe(1);
    expect(await cache.get("t2")).not.toBeNull();
  });

  it("deleteMany on an empty list touches nothing", async () => {
    await cache.put(doc("t1"));
    await cache.deleteMany([]);
    expect(await cache.count()).toBe(1);
  });

  it("wipe drops everything and leaves the instance usable", async () => {
    await cache.put(doc("t1"));
    await cache.wipe();
    expect(await cache.count()).toBe(0);
    await cache.put(doc("t2"));
    expect(await cache.get("t2")).not.toBeNull();
  });
});
