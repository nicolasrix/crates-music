// Ownership tagging: a same-user logout/login keeps the offline cache and
// preferences, a different user signing in still gets a clean slate.

import "fake-indexeddb/auto";
import { beforeEach, describe, expect, it } from "vitest";

import { getAudioCache } from "../cache/audioCache";
import { clearLocalOwner, readLocalOwner, reconcileLocalOwner, writeLocalOwner } from "./localOwner";
import { clearUserData } from "./userData";

// The vitest `node` env has no localStorage; mirrors userData.test.ts.
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

const KEY = { trackId: "t1", bitrate: null, codec: "mp3" } as const;

async function seedDownload(): Promise<void> {
  await getAudioCache().put(KEY, new Blob([new Uint8Array(10)]));
}

describe("reconcileLocalOwner", () => {
  beforeEach(async () => {
    localStorage.clear();
    await clearUserData();
  });

  it("keeps downloads and settings when the same user signs back in", async () => {
    await seedDownload();
    localStorage.setItem("crates-music.cache.settings", '{"pinnedBudgetBytes":1}');
    writeLocalOwner(7);

    expect(await reconcileLocalOwner(7)).toBe(false);

    // The regression this whole module exists for: a logout/login cycle by
    // the same person must not cost them their offline library.
    expect(await getAudioCache().getMetaByTrack("t1")).not.toBeNull();
    expect(localStorage.getItem("crates-music.cache.settings")).toBe('{"pinnedBudgetBytes":1}');
  });

  it("wipes when a different user signs in", async () => {
    await seedDownload();
    localStorage.setItem("crates-music.cache.settings", '{"pinnedBudgetBytes":1}');
    writeLocalOwner(7);

    expect(await reconcileLocalOwner(8)).toBe(true);

    expect(await getAudioCache().getMetaByTrack("t1")).toBeNull();
    expect(localStorage.getItem("crates-music.cache.settings")).toBeNull();
    expect(readLocalOwner()).toBe(8);
  });

  it("adopts an untagged cache instead of wiping it", async () => {
    // Upgrade path: data written before tagging existed belongs to whoever
    // was last signed in here, because the old build wiped on sign-out.
    await seedDownload();
    clearLocalOwner();

    expect(await reconcileLocalOwner(7)).toBe(false);

    expect(await getAudioCache().getMetaByTrack("t1")).not.toBeNull();
    expect(readLocalOwner()).toBe(7);
  });

  it("keeps the owner tag outside the swept preference namespace", async () => {
    // Load-bearing: if a wipe erased the tag, "wiped" would look identical
    // to "never tagged", and untagged means adopt.
    writeLocalOwner(7);
    await clearUserData();
    expect(readLocalOwner()).toBe(7);
  });
});
