// Offline lyrics, stored beside the offline audio.
//
// Its own IndexedDB database rather than a store inside the audio cache's:
// the two have nothing in common at the engine level. Audio is bytes under
// a two-budget LRU keyed by (trackId, bitrate, codec); lyrics are a few KB
// of text keyed by trackId alone, with no budget and no eviction of their
// own. Sharing a schema would mean the audio cache's version number moves
// whenever the lyrics shape changes, and every reader of `CacheDB` would
// have to skip past a store it never touches.
//
// What the two *do* share is a lifetime, and that is wired explicitly
// rather than structurally: AudioCacheContext captures lyrics when a track
// is downloaded and deletes them when its audio is unpinned or evicted, so
// this store never outlives what it annotates. Its upper bound is therefore
// the number of downloaded tracks, which is why there is no eviction pass
// here — the audio budget already caps it.
//
// Only downloaded tracks land here. Lyrics you merely *looked at* live in
// TanStack Query's in-memory cache for the session and are re-fetched
// afterwards; persisting those too would grow without bound and would send
// the whole listening history's worth of titles to the provider, which is
// exactly what `[lyrics] external_lookup` exists to let you refuse.

import { openDB, type DBSchema, type IDBPDatabase } from "idb";

import type { LyricsDoc } from "../api/lyrics";

interface LyricsRecord {
  trackId: string;
  doc: LyricsDoc;
  /** When this copy was written locally — distinct from the document's own
   *  `fetched_at`, which is when the *gateway* resolved it. */
  storedMs: number;
}

interface LyricsDB extends DBSchema {
  lyrics: { key: string; value: LyricsRecord };
}

interface LyricsCacheOptions {
  dbName?: string;
  now?: () => number;
}

export class LyricsCache {
  private readonly dbName: string;
  private readonly now: () => number;
  private dbPromise: Promise<IDBPDatabase<LyricsDB>> | null = null;

  constructor(opts: LyricsCacheOptions = {}) {
    this.dbName = opts.dbName ?? "crates-music-lyrics";
    this.now = opts.now ?? (() => Date.now());
  }

  private db(): Promise<IDBPDatabase<LyricsDB>> {
    if (!this.dbPromise) {
      this.dbPromise = openDB<LyricsDB>(this.dbName, 1, {
        upgrade(db) {
          db.createObjectStore("lyrics", { keyPath: "trackId" });
        },
      });
    }
    return this.dbPromise;
  }

  async get(trackId: string): Promise<LyricsDoc | null> {
    const r = await (await this.db()).get("lyrics", trackId);
    return r?.doc ?? null;
  }

  async put(doc: LyricsDoc): Promise<void> {
    await (await this.db()).put("lyrics", {
      trackId: doc.track_id,
      doc,
      storedMs: this.now(),
    });
  }

  /** Overwrite an existing copy, or do nothing. The read path calls this on
   *  every successful network fetch, so a downloaded track's offline lyrics
   *  track the server's answer (a refresh that fixed a wrong match reaches
   *  the offline copy too) without every *browsed* track creating a row. */
  async refreshIfPresent(doc: LyricsDoc): Promise<boolean> {
    const db = await this.db();
    const existing = await db.get("lyrics", doc.track_id);
    if (!existing) return false;
    await db.put("lyrics", { trackId: doc.track_id, doc, storedMs: this.now() });
    return true;
  }

  async delete(trackId: string): Promise<void> {
    await (await this.db()).delete("lyrics", trackId);
  }

  async deleteMany(trackIds: readonly string[]): Promise<void> {
    if (!trackIds.length) return;
    const db = await this.db();
    const tx = db.transaction("lyrics", "readwrite");
    for (const id of trackIds) void tx.store.delete(id);
    await tx.done;
  }

  async count(): Promise<number> {
    return (await this.db()).count("lyrics");
  }

  /** Drop the whole database — called on sign-out alongside the audio wipe.
   *  Mirrors AudioCache.wipe(), including resolving on `blocked` so a
   *  background tab holding a connection cannot hang sign-out. */
  async wipe(): Promise<void> {
    if (this.dbPromise) {
      try {
        (await this.dbPromise).close();
      } catch {
        /* open failed or already closed — the delete below still runs */
      }
      this.dbPromise = null;
    }
    await new Promise<void>((resolve, reject) => {
      const req = indexedDB.deleteDatabase(this.dbName);
      req.onsuccess = () => resolve();
      req.onerror = () => reject(req.error ?? new Error("deleteDatabase failed"));
      req.onblocked = () => resolve();
    });
  }
}

let singleton: LyricsCache | null = null;

/** Process-wide instance used by the app. Tests construct their own. */
export function getLyricsCache(): LyricsCache {
  if (!singleton) singleton = new LyricsCache();
  return singleton;
}
