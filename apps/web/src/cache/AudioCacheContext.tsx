// Bridges the IndexedDB audio cache (audioCache.ts) to playback and the UI.
//
// The hard constraint: PlayerContext.primePlayback sets audio.src + play()
// *synchronously* inside a click handler (the browser's autoplay policy
// refuses a deferred play()). IndexedDB reads are async. We square that with
// an in-memory Map<trackId, blob:URL> that is warmed ahead of time for the
// queue window, so resolveSrc() can answer synchronously. The natural-advance
// path (a sync-state effect, no live gesture) awaits ensureUrl() directly.
//
// Object-URL lifetime: created on demand, revoked when a track leaves the
// prefetch window or when the cache evicts it. The currently-playing track is
// never revoked. The underlying Blob always lives in IndexedDB regardless.

import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import { fetchTrackBlob, streamUrl } from "../api/client";
import { useSync } from "../sync/SyncContext";
import {
  type AudioCacheStats,
  type AudioEntry,
  getAudioCache,
  type PinOutcome,
} from "./audioCache";
import { loadCacheSettings, qualityParams } from "./cacheSettings";

// Fetch a track at the user's configured offline quality and return the
// triple to store it under. Settings are read at call time (cheap
// localStorage read) so a quality change applies to the next fetch without
// a reload. Already-cached variants are never re-fetched or migrated.
async function fetchAtConfiguredQuality(trackId: string) {
  const q = qualityParams(loadCacheSettings().downloadQuality);
  const { blob, codec } = await fetchTrackBlob(trackId, q);
  return { blob, codec, bitrate: q ? q.maxBitRate : null };
}

// How many upcoming queue items to pre-resolve into blob URLs (plus the one
// behind, for a quick prev). Bounds the number of live object URLs / pinned
// Blobs held in memory at once.
const PREFETCH_AHEAD = 3;
const PREFETCH_BEHIND = 1;

interface AudioCacheCtx {
  /** Synchronous: a blob: URL if already warmed, else the network stream URL.
   *  Safe to call from a click handler. */
  resolveSrc: (trackId: string) => string;
  /** Async: ensure a blob: URL exists for a cached track (null if uncached). */
  ensureUrl: (trackId: string) => Promise<string | null>;
  /** Record that a track was played: LRU-touch on hit, or fetch-and-cache the
   *  miss (regular budget) when online. Fire-and-forget. */
  notePlayed: (trackId: string) => void;
  /** Pin a track for offline ("download"): fetch if needed, then pin. */
  download: (trackId: string) => Promise<PinOutcome>;
  /** Unpin a downloaded track (the blob stays under the LRU budget). */
  removeDownload: (trackId: string) => Promise<void>;
  isDownloaded: (trackId: string) => boolean;
  downloadedIds: ReadonlySet<string>;
  /** Fit the regular budget now (mirrors CLI `cache evict`). */
  evictToBudget: () => Promise<void>;
  stats: () => Promise<AudioCacheStats>;
  listPinned: () => Promise<AudioEntry[]>;
  /** Increments on any cache mutation; consumers refetch stats on change. */
  revision: number;
}

const Ctx = createContext<AudioCacheCtx | null>(null);

export function AudioCacheProvider({ children }: { children: ReactNode }) {
  const cache = useMemo(() => getAudioCache(), []);
  const { state } = useSync();
  const items = state.playback.queue.items;
  const cursor = state.playback.now_playing_index;

  // trackId -> object URL. A ref (not state) because playback reads it
  // synchronously and we don't want renders on every warm/revoke.
  const urls = useRef<Map<string, string>>(new Map());
  // Dedup concurrent ensureUrl / cache-on-play for the same track.
  const urlInflight = useRef<Map<string, Promise<string | null>>>(new Map());
  const fetchInflight = useRef<Set<string>>(new Set());

  const [downloadedIds, setDownloadedIds] = useState<ReadonlySet<string>>(new Set());
  const [revision, setRevision] = useState(0);
  const bump = useCallback(() => setRevision((r) => r + 1), []);

  const refreshDownloaded = useCallback(async () => {
    const pinned = await cache.listPinned();
    setDownloadedIds(new Set(pinned.map((e) => e.trackId)));
    bump();
  }, [cache, bump]);

  const revoke = useCallback((trackId: string) => {
    const u = urls.current.get(trackId);
    if (u) {
      URL.revokeObjectURL(u);
      urls.current.delete(trackId);
    }
  }, []);

  const ensureUrl = useCallback(
    (trackId: string): Promise<string | null> => {
      const existing = urls.current.get(trackId);
      if (existing) return Promise.resolve(existing);
      const pending = urlInflight.current.get(trackId);
      if (pending) return pending;
      const p = (async () => {
        const meta = await cache.getMetaByTrack(trackId);
        if (!meta) return null;
        const blob = await cache.getBlob(meta.key);
        if (!blob) return null;
        // A racing call may have won; reuse its URL.
        const won = urls.current.get(trackId);
        if (won) return won;
        const url = URL.createObjectURL(blob);
        urls.current.set(trackId, url);
        return url;
      })().finally(() => urlInflight.current.delete(trackId));
      urlInflight.current.set(trackId, p);
      return p;
    },
    [cache],
  );

  const resolveSrc = useCallback(
    (trackId: string): string => urls.current.get(trackId) ?? streamUrl(trackId),
    [],
  );

  const cacheOnPlay = useCallback(
    async (trackId: string) => {
      if (typeof navigator !== "undefined" && navigator.onLine === false) return;
      if (fetchInflight.current.has(trackId)) return;
      fetchInflight.current.add(trackId);
      try {
        const { blob, codec, bitrate } = await fetchAtConfiguredQuality(trackId);
        await cache.put({ trackId, bitrate, codec }, blob);
        bump();
      } catch {
        /* offline / auth / network — leave the track uncached */
      } finally {
        fetchInflight.current.delete(trackId);
      }
    },
    [cache, bump],
  );

  const notePlayed = useCallback(
    (trackId: string) => {
      void (async () => {
        const meta = await cache.getMetaByTrack(trackId);
        if (meta) {
          await cache.touch(meta.key);
          return;
        }
        await cacheOnPlay(trackId);
      })();
    },
    [cache, cacheOnPlay],
  );

  const download = useCallback(
    async (trackId: string): Promise<PinOutcome> => {
      let meta = await cache.getMetaByTrack(trackId);
      if (!meta) {
        // Fetch then store; let a network failure propagate to the caller.
        const { blob, codec, bitrate } = await fetchAtConfiguredQuality(trackId);
        meta = await cache.put({ trackId, bitrate, codec }, blob);
      }
      const outcome = await cache.pin(meta.key);
      await refreshDownloaded();
      return outcome;
    },
    [cache, refreshDownloaded],
  );

  const removeDownload = useCallback(
    async (trackId: string) => {
      const meta = await cache.getMetaByTrack(trackId);
      if (!meta) return;
      await cache.unpin(meta.key);
      await refreshDownloaded();
    },
    [cache, refreshDownloaded],
  );

  const isDownloaded = useCallback(
    (trackId: string) => downloadedIds.has(trackId),
    [downloadedIds],
  );

  const evictToBudget = useCallback(async () => {
    await cache.evictLruToFit();
    bump();
  }, [cache, bump]);

  const stats = useCallback(() => cache.stats(), [cache]);
  const listPinned = useCallback(() => cache.listPinned(), [cache]);

  // Revoke object URLs for tracks the cache evicts, and keep downloaded set
  // honest if a delete touched a pinned row.
  useEffect(() => {
    const off = cache.onDelete((trackIds) => {
      for (const id of trackIds) revoke(id);
      void refreshDownloaded();
    });
    return off;
  }, [cache, revoke, refreshDownloaded]);

  // Initial downloaded set.
  useEffect(() => {
    void refreshDownloaded();
  }, [refreshDownloaded]);

  // Warm the prefetch window; revoke URLs that fall outside it (never the
  // currently-playing track).
  useEffect(() => {
    if (cursor === null) return;
    const window = new Set<string>();
    for (let j = cursor - PREFETCH_BEHIND; j <= cursor + PREFETCH_AHEAD; j++) {
      const it = items[j];
      if (it) window.add(it.track_id);
    }
    const currentId = items[cursor]?.track_id;
    for (const id of window) void ensureUrl(id);
    for (const id of [...urls.current.keys()]) {
      if (!window.has(id) && id !== currentId) revoke(id);
    }
  }, [items, cursor, ensureUrl, revoke]);

  // Revoke everything on unmount.
  useEffect(() => {
    const map = urls.current;
    return () => {
      for (const u of map.values()) URL.revokeObjectURL(u);
      map.clear();
    };
  }, []);

  const value = useMemo<AudioCacheCtx>(
    () => ({
      resolveSrc,
      ensureUrl,
      notePlayed,
      download,
      removeDownload,
      isDownloaded,
      downloadedIds,
      evictToBudget,
      stats,
      listPinned,
      revision,
    }),
    [
      resolveSrc,
      ensureUrl,
      notePlayed,
      download,
      removeDownload,
      isDownloaded,
      downloadedIds,
      evictToBudget,
      stats,
      listPinned,
      revision,
    ],
  );

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useAudioCache(): AudioCacheCtx {
  const v = useContext(Ctx);
  if (!v) throw new Error("useAudioCache must be used inside <AudioCacheProvider>");
  return v;
}
