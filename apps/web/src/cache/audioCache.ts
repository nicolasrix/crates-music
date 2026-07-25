// L3 audio cache for the web client — an IndexedDB reimplementation of
// the contract in crates/music-cache/src/audio.rs. Stores whole-file audio
// blobs (passthrough original) addressed by AudioKey, with a two-budget
// LRU: a "regular" budget (auto-cached recents, LRU-evicted) and a
// separate "pinned" budget (explicit downloads, never LRU-evicted).
//
// Why IndexedDB blobs + URL.createObjectURL rather than a Service Worker +
// Cache API: the gateway stream endpoint is pure passthrough with no HTTP
// Range support, so a SW would have to synthesise 206 responses by hand.
// Blob URLs let the browser seek locally against a stored whole file.
//
// Invariants mirrored from the Rust impl:
//   • eviction runs after every put, after unpin, and after a budget drop
//   • a pinned entry is never an LRU victim
//   • the single remaining regular row is never evicted ("user just asked
//     for it, even if it alone exceeds budget")
//   • put preserves an existing pinned flag (re-fetching keeps a download)

import { openDB, type DBSchema, type IDBPDatabase } from "idb";

import { type AudioKey, canonicalKey } from "./audioKey";
import { type CacheBudgets, loadCacheSettings } from "./cacheSettings";

export interface AudioEntry {
  key: string;
  trackId: string;
  bitrate: number | null;
  codec: string;
  bytes: number;
  lastAccessedMs: number;
  pinned: boolean;
}

export interface AudioCacheStats {
  regularCount: number;
  regularBytes: number;
  regularBudgetBytes: number;
  pinnedCount: number;
  pinnedBytes: number;
  pinnedBudgetBytes: number;
  /** Browser-reported origin storage usage/quota (best effort). */
  storageUsedBytes?: number;
  storageQuotaBytes?: number;
  /** Whether the origin has been granted persistent storage. */
  persisted?: boolean;
}

export type PinOutcome =
  | { kind: "pinned" }
  | { kind: "already-pinned" }
  | { kind: "not-in-cache" }
  | { kind: "would-exceed-budget"; overBy: number };

export type UnpinOutcome =
  | { kind: "unpinned" }
  | { kind: "not-pinned" }
  | { kind: "not-in-cache" };

// Stored shape. `pinned` is 0/1 (matches the Rust integer column and keeps
// the value a valid future index key).
interface MetaRecord {
  key: string;
  trackId: string;
  bitrate: number | null;
  codec: string;
  bytes: number;
  lastAccessedMs: number;
  pinned: 0 | 1;
}

interface CacheDB extends DBSchema {
  blobs: { key: string; value: Blob };
  meta: {
    key: string;
    value: MetaRecord;
    indexes: { byTrack: string };
  };
}

function toEntry(r: MetaRecord): AudioEntry {
  return {
    key: r.key,
    trackId: r.trackId,
    bitrate: r.bitrate,
    codec: r.codec,
    bytes: r.bytes,
    lastAccessedMs: r.lastAccessedMs,
    pinned: r.pinned === 1,
  };
}

interface AudioCacheOptions {
  dbName?: string;
  /** Read live budgets on each call so Settings changes are picked up. */
  budgets?: () => CacheBudgets;
  /** Injectable clock — tests use a monotonic counter for deterministic LRU. */
  now?: () => number;
}

export class AudioCache {
  private readonly dbName: string;
  private readonly budgets: () => CacheBudgets;
  private readonly now: () => number;
  private dbPromise: Promise<IDBPDatabase<CacheDB>> | null = null;
  private persistTried = false;
  private readonly deleteListeners = new Set<(trackIds: string[]) => void>();

  constructor(opts: AudioCacheOptions = {}) {
    this.dbName = opts.dbName ?? "crates-music-audio";
    this.budgets = opts.budgets ?? loadCacheSettings;
    this.now = opts.now ?? (() => Date.now());
  }

  /** Subscribe to entry deletions (eviction / explicit delete). The
   *  playback layer uses this to revoke object URLs. Returns an
   *  unsubscribe fn. */
  onDelete(fn: (trackIds: string[]) => void): () => void {
    this.deleteListeners.add(fn);
    return () => this.deleteListeners.delete(fn);
  }

  private emitDelete(trackIds: string[]): void {
    if (!trackIds.length) return;
    for (const fn of this.deleteListeners) {
      try {
        fn(trackIds);
      } catch {
        /* a misbehaving listener must not break cache writes */
      }
    }
  }

  private db(): Promise<IDBPDatabase<CacheDB>> {
    if (!this.dbPromise) {
      this.dbPromise = openDB<CacheDB>(this.dbName, 1, {
        upgrade(db) {
          db.createObjectStore("blobs");
          const meta = db.createObjectStore("meta", { keyPath: "key" });
          meta.createIndex("byTrack", "trackId");
        },
      });
    }
    void this.requestPersist();
    return this.dbPromise;
  }

  private async requestPersist(): Promise<void> {
    if (this.persistTried) return;
    this.persistTried = true;
    try {
      if (typeof navigator !== "undefined" && navigator.storage?.persist) {
        await navigator.storage.persist();
      }
    } catch {
      /* persistence is best-effort; carry on unpersisted */
    }
  }

  async getMeta(key: string): Promise<AudioEntry | null> {
    const db = await this.db();
    const r = await db.get("meta", key);
    return r ? toEntry(r) : null;
  }

  /** Find the (single, for v1) cached entry for a track id. */
  async getMetaByTrack(trackId: string): Promise<AudioEntry | null> {
    const db = await this.db();
    const r = await db.getFromIndex("meta", "byTrack", trackId);
    return r ? toEntry(r) : null;
  }

  async getBlob(key: string): Promise<Blob | null> {
    const db = await this.db();
    return (await db.get("blobs", key)) ?? null;
  }

  /** Write a blob + metadata, preserving any existing pinned flag, then
   *  fit the regular budget. Returns the stored entry. */
  async put(k: AudioKey, blob: Blob): Promise<AudioEntry> {
    const db = await this.db();
    const key = canonicalKey(k);
    const existing = await db.get("meta", key);
    const rec: MetaRecord = {
      key,
      trackId: k.trackId,
      bitrate: k.bitrate,
      codec: k.codec,
      bytes: blob.size,
      lastAccessedMs: this.now(),
      pinned: existing?.pinned ?? 0,
    };
    const tx = db.transaction(["blobs", "meta"], "readwrite");
    await Promise.all([
      tx.objectStore("blobs").put(blob, key),
      tx.objectStore("meta").put(rec),
      tx.done,
    ]);
    await this.evictLruToFit();
    return toEntry(rec);
  }

  /** Bump last-accessed (LRU). Does not trigger eviction. */
  async touch(key: string): Promise<boolean> {
    const db = await this.db();
    const r = await db.get("meta", key);
    if (!r) return false;
    r.lastAccessedMs = this.now();
    await db.put("meta", r);
    return true;
  }

  async pin(key: string): Promise<PinOutcome> {
    const db = await this.db();
    const r = await db.get("meta", key);
    if (!r) return { kind: "not-in-cache" };
    if (r.pinned === 1) return { kind: "already-pinned" };
    const agg = await this.aggregate();
    const projected = agg.pinnedBytes + r.bytes;
    const budget = this.budgets().pinnedBudgetBytes;
    if (projected > budget) {
      return { kind: "would-exceed-budget", overBy: projected - budget };
    }
    r.pinned = 1;
    await db.put("meta", r);
    return { kind: "pinned" };
  }

  async unpin(key: string): Promise<UnpinOutcome> {
    const db = await this.db();
    const r = await db.get("meta", key);
    if (!r) return { kind: "not-in-cache" };
    if (r.pinned === 0) return { kind: "not-pinned" };
    r.pinned = 0;
    await db.put("meta", r);
    // newly-unpinned bytes now count against the regular budget.
    await this.evictLruToFit();
    return { kind: "unpinned" };
  }

  async listPinned(): Promise<AudioEntry[]> {
    const db = await this.db();
    const all = await db.getAll("meta");
    return all
      .filter((r) => r.pinned === 1)
      .map(toEntry)
      .sort((a, b) => (a.trackId < b.trackId ? -1 : a.trackId > b.trackId ? 1 : 0));
  }

  /** Hard-delete a single entry regardless of pinned state. */
  async delete(key: string): Promise<boolean> {
    const db = await this.db();
    const r = await db.get("meta", key);
    if (!r) return false;
    const tx = db.transaction(["blobs", "meta"], "readwrite");
    await Promise.all([
      tx.objectStore("blobs").delete(key),
      tx.objectStore("meta").delete(key),
      tx.done,
    ]);
    this.emitDelete([r.trackId]);
    return true;
  }

  /** Delete the entire cache database — every blob, pin, and meta row —
   *  and reset this instance so the next call re-opens a fresh DB. Used on
   *  logout / account-switch so one user's offline library (and pinned
   *  tracks) can't bleed into whoever signs in next on a shared device
   *  (sec review 1.6). */
  async wipe(): Promise<void> {
    if (this.dbPromise) {
      try {
        (await this.dbPromise).close();
      } catch {
        /* open failed or already closed — the delete below still runs */
      }
      this.dbPromise = null;
      this.persistTried = false;
    }
    await new Promise<void>((resolve, reject) => {
      const req = indexedDB.deleteDatabase(this.dbName);
      req.onsuccess = () => resolve();
      req.onerror = () => reject(req.error ?? new Error("deleteDatabase failed"));
      // Another tab holding a connection blocks deletion until it closes;
      // resolve anyway so logout isn't hung waiting on a background tab.
      req.onblocked = () => resolve();
    });
  }

  async stats(): Promise<AudioCacheStats> {
    const agg = await this.aggregate();
    const b = this.budgets();
    const out: AudioCacheStats = {
      regularCount: agg.regularCount,
      regularBytes: agg.regularBytes,
      regularBudgetBytes: b.regularBudgetBytes,
      pinnedCount: agg.pinnedCount,
      pinnedBytes: agg.pinnedBytes,
      pinnedBudgetBytes: b.pinnedBudgetBytes,
    };
    try {
      if (typeof navigator !== "undefined" && navigator.storage) {
        if (navigator.storage.estimate) {
          const est = await navigator.storage.estimate();
          if (typeof est.usage === "number") out.storageUsedBytes = est.usage;
          if (typeof est.quota === "number") out.storageQuotaBytes = est.quota;
        }
        if (navigator.storage.persisted) {
          out.persisted = await navigator.storage.persisted();
        }
      }
    } catch {
      /* estimate/persisted are best-effort */
    }
    return out;
  }

  /** Evict least-recently-used regular entries until the regular budget is
   *  met, always leaving at least one regular row. Returns evicted entries. */
  async evictLruToFit(): Promise<AudioEntry[]> {
    const db = await this.db();
    const all = await db.getAll("meta");
    const regular = all
      .filter((r) => r.pinned === 0)
      .sort((a, b) => a.lastAccessedMs - b.lastAccessedMs || (a.key < b.key ? -1 : 1));
    const budget = this.budgets().regularBudgetBytes;
    let total = regular.reduce((s, r) => s + r.bytes, 0);

    const victims: MetaRecord[] = [];
    let i = 0;
    while (total > budget && regular.length - victims.length > 1 && i < regular.length) {
      const v = regular[i++]!;
      victims.push(v);
      total -= v.bytes;
    }
    if (!victims.length) return [];

    const tx = db.transaction(["blobs", "meta"], "readwrite");
    for (const v of victims) {
      void tx.objectStore("blobs").delete(v.key);
      void tx.objectStore("meta").delete(v.key);
    }
    await tx.done;

    const entries = victims.map(toEntry);
    this.emitDelete(entries.map((e) => e.trackId));
    return entries;
  }

  private async aggregate(): Promise<{
    regularBytes: number;
    regularCount: number;
    pinnedBytes: number;
    pinnedCount: number;
  }> {
    const db = await this.db();
    const all = await db.getAll("meta");
    let regularBytes = 0;
    let regularCount = 0;
    let pinnedBytes = 0;
    let pinnedCount = 0;
    for (const r of all) {
      if (r.pinned === 1) {
        pinnedBytes += r.bytes;
        pinnedCount += 1;
      } else {
        regularBytes += r.bytes;
        regularCount += 1;
      }
    }
    return { regularBytes, regularCount, pinnedBytes, pinnedCount };
  }
}

let singleton: AudioCache | null = null;

/** Process-wide cache instance used by the app (budgets read from
 *  localStorage on each call). Tests construct their own instances. */
export function getAudioCache(): AudioCache {
  if (!singleton) singleton = new AudioCache();
  return singleton;
}
