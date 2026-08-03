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
import { downloadTargets } from "./prefetchWindow";
import { useOnline } from "./useOnline";

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
// Blobs held in memory at once. Resolving is cheap — it only makes URLs for
// blobs that are *already* stored.
const PREFETCH_AHEAD = 3;
const PREFETCH_BEHIND = 1;

// How many upcoming queue items to actually *download*.
//
// Deliberately smaller than the resolve window: this spends bandwidth on
// tracks the user may still skip past, and one track ahead is enough to make
// the next advance play from a blob. Keeping it at 1 means the download pass
// never moves more bytes than the old cache-on-play did — it just moves them
// early enough to replace the streaming fetch instead of duplicating it.
const DOWNLOAD_AHEAD = 1;

// Let the queue window settle before spending bandwidth. Skipping through six
// tracks used to fire six downloads; now only wherever you land survives the
// debounce. Also keeps the pass from competing with the *current* track's
// first seconds, which is when a slow link hurts most.
const DOWNLOAD_SETTLE_MS = 3_000;

interface AudioCacheCtx {
  /** Synchronous: a blob: URL if already warmed, else the network stream URL.
   *  Safe to call from a click handler. */
  resolveSrc: (trackId: string) => string;
  /** Async: ensure a blob: URL exists for a cached track (null if uncached). */
  ensureUrl: (trackId: string) => Promise<string | null>;
  /** Record that a track was played: LRU-touch on hit, or fetch-and-cache the
   *  miss (regular budget) when online. Fire-and-forget. */
  notePlayed: (trackId: string) => void;
  /** Warm a track into the regular (auto-evicted) budget: LRU-touch on hit,
   *  fetch-and-`put` on miss. Unlike `download`, never pins — overflow rolls
   *  off LRU like any auto-cached track. Awaitable, and throws on fetch
   *  failure so bulk callers can count misses. */
  cacheTrack: (trackId: string) => Promise<"fetched" | "hit">;
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
  // Gates the download pass, and re-triggers it when connectivity returns
  // to a queue that hasn't otherwise changed.
  const online = useOnline();

  // trackId -> object URL. A ref (not state) because playback reads it
  // synchronously and we don't want renders on every warm/revoke.
  const urls = useRef<Map<string, string>>(new Map());
  // Dedup concurrent ensureUrl / download for the same track.
  const urlInflight = useRef<Map<string, Promise<string | null>>>(new Map());
  const fetchInflight = useRef<Map<string, Promise<"fetched" | "hit">>>(new Map());

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

  /** Is this track backed by a warmed blob: URL? If not, `resolveSrc` handed
   *  the element a network stream URL and the browser is fetching it now. */
  const hasUrl = useCallback((trackId: string) => urls.current.has(trackId), []);

  const notePlayed = useCallback(
    (trackId: string) => {
      void (async () => {
        const meta = await cache.getMetaByTrack(trackId);
        if (meta) await cache.touch(meta.key);
        // A miss is deliberately *not* fetched here. It used to be, which
        // meant every uncached track was pulled twice at once — once by the
        // <audio> element streaming it and once by this. The download pass
        // below fetches around the cursor instead, early enough that the
        // next advance plays from the blob rather than the network.
      })();
    },
    [cache],
  );

  const cacheTrack = useCallback(
    (trackId: string): Promise<"fetched" | "hit"> => {
      // Deduped like ensureUrl: the download pass, a bulk "cache liked"
      // sweep and a manual download can all name the same track, and each
      // extra fetch is a whole audio file.
      const pending = fetchInflight.current.get(trackId);
      if (pending) return pending;
      const p = (async (): Promise<"fetched" | "hit"> => {
        const meta = await cache.getMetaByTrack(trackId);
        if (meta) {
          await cache.touch(meta.key);
          return "hit";
        }
        const { blob, codec, bitrate } = await fetchAtConfiguredQuality(trackId);
        await cache.put({ trackId, bitrate, codec }, blob);
        bump();
        return "fetched";
      })().finally(() => fetchInflight.current.delete(trackId));
      fetchInflight.current.set(trackId, p);
      return p;
    },
    [cache, bump],
  );

  const download = useCallback(
    async (trackId: string): Promise<PinOutcome> => {
      // Routed through cacheTrack so a download pass already fetching this
      // track is joined rather than raced. A network failure still
      // propagates to the caller, which is what the UI reports.
      await cacheTrack(trackId);
      const meta = await cache.getMetaByTrack(trackId);
      if (!meta) throw new Error(`download: ${trackId} was not stored`);
      const outcome = await cache.pin(meta.key);
      await refreshDownloaded();
      return outcome;
    },
    [cache, cacheTrack, refreshDownloaded],
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

  // Download pass: pull the tracks around the cursor into the cache so the
  // *next* advance plays from a blob instead of fetching over the network
  // again.
  //
  // This replaces cache-on-play, which fetched a track at the same moment the
  // <audio> element was streaming it — two full downloads of the same song,
  // racing each other for the link. Measured on mobile data 2026-08-03: two
  // 2,026,005-byte requests for one 2 MB track, one carrying `access_token`
  // (the element) and one not (the cache). Fetching one track *ahead* costs
  // the same bytes as fetching the current one late, but they replace the
  // streaming fetch instead of duplicating it.
  //
  // Deliberately skipped: the track the element is streaming right now (in
  // the window but not blob-backed). Downloading that one is exactly the
  // duplicate this pass exists to remove; it becomes eligible on the next
  // advance, by which point its stream has finished and PREFETCH_BEHIND
  // still covers it.
  useEffect(() => {
    if (cursor === null) return;
    if (!online) return;
    const timer = window.setTimeout(() => {
      const targets = downloadTargets(
        items,
        cursor,
        { behind: PREFETCH_BEHIND, ahead: DOWNLOAD_AHEAD },
        hasUrl,
      );
      // Failures are expected and uninteresting (offline mid-pass, auth
      // blip): the track simply stays uncached and the next pass retries.
      for (const id of targets) void cacheTrack(id).catch(() => {});
    }, DOWNLOAD_SETTLE_MS);
    return () => clearTimeout(timer);
  }, [items, cursor, online, cacheTrack, hasUrl]);

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
      cacheTrack,
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
      cacheTrack,
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
