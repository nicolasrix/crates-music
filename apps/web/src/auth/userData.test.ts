// clearUserData() sweeps the offline cache + preference keys on account
// switch (sec review 1.6), while leaving non-user keys (RUM id) intact.

import "fake-indexeddb/auto";
import { beforeEach, describe, expect, it } from "vitest";

import { getAudioCache } from "../cache/audioCache";
import { clearUserData } from "./userData";

// The vitest `node` env has no localStorage; install a minimal in-memory
// shim (the app only needs get/set/remove/length/key). Mirrors how
// `fake-indexeddb/auto` patches the IDB globals above.
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

describe("clearUserData", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("removes preference keys but keeps non-user keys", async () => {
    // User preferences (must be wiped).
    localStorage.setItem("crates-music.theme", "dark");
    localStorage.setItem("crates-music.cache.settings", "{}");
    localStorage.setItem("crates-music.autoplay.settings", "{}");
    localStorage.setItem("player.volume", "0.5");
    // Not user data (must survive).
    localStorage.setItem("rum.session_id", "abc123");

    await clearUserData();

    expect(localStorage.getItem("crates-music.theme")).toBeNull();
    expect(localStorage.getItem("crates-music.cache.settings")).toBeNull();
    expect(localStorage.getItem("crates-music.autoplay.settings")).toBeNull();
    expect(localStorage.getItem("player.volume")).toBeNull();
    // The RUM correlation id is not user data and is left alone.
    expect(localStorage.getItem("rum.session_id")).toBe("abc123");
  });

  it("wipes the offline audio cache", async () => {
    const cache = getAudioCache();
    await cache.put({ trackId: "t1", bitrate: null, codec: "mp3" }, new Blob([new Uint8Array(10)]));
    expect(await cache.getMetaByTrack("t1")).not.toBeNull();

    await clearUserData();

    expect(await cache.getMetaByTrack("t1")).toBeNull();
  });
});
