// AutoplayContext: owns the "autoplay" toggle and the queue-refill
// effect. When autoplay is on, the effect tops up the upcoming queue
// to MIN_UPCOMING tracks by pulling recommendations from the gateway's
// /v1/recommend/next endpoint.
//
// Why a separate context (rather than folding into PlayerContext):
// autoplay is a recommendation-policy concern, not a playback concern,
// and decoupling makes the file diffs cleaner. Both PlayerBar and the
// refill effect read the same `autoplay` flag through this hook.
//
// Persistence: the flag is stored in localStorage so it survives
// reloads. SSR is not in play (this is a Vite SPA) so the lazy
// initializer reading localStorage on first render is safe.

import {
  createContext,
  ReactNode,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
} from "react";
import { startStationFromAny } from "../api/recommend";
import { useSync } from "../sync/SyncContext";
import type { Track } from "../api/types";

// Threshold the queue refill targets. The gateway returns up to N
// recommendations; we ask for more than we strictly need so dedup
// against the existing queue, the artist cap, and occasional `getSong`
// 404s don't leave us short.
const MIN_UPCOMING = 5;
const FETCH_BUFFER = MIN_UPCOMING * 4;
const STORAGE_KEY = "crates-music.autoplay";
// Hold the in-flight lock for a beat after pushing so the gateway WS
// round-trip can land before the effect re-evaluates. Without this the
// effect can re-fire while the new items haven't shown up in local
// state yet, double-pushing recommendations.
const REFILL_COOLDOWN_MS = 1500;
// When the recommender genuinely cannot supply more variety from the
// current seeds (everything similar to the seed is already in the
// queue), bumping the cooldown prevents a busy loop. The next natural
// cursor advance will change the seed pool and unblock progress.
const UNDERDELIVERY_COOLDOWN_MS = 30_000;
// Cap on tracks per artist in the queue. Keeps autoplay from
// converging on a single artist's catalog when the user has many
// records by them (recommender naturally clusters on similar tracks,
// which for a heavily-represented artist means more of the same).
const MAX_PER_ARTIST = 2;

function normalizeTitle(title: string): string {
  // Lowercase + strip parentheticals/brackets ("(Album Version)",
  // "[Remastered]", etc.). Catches single-vs-album dupes that share
  // a base title and differ only in the version qualifier.
  return title
    .toLowerCase()
    .replace(/\s*[(\[][^)\]]*[)\]]\s*/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

function artistKey(t: Track | undefined): string | null {
  if (!t) return null;
  if (t.artistId) return `id:${t.artistId}`;
  const name = (t.artist ?? "").trim().toLowerCase();
  return name ? `name:${name}` : null;
}

function dedupeKey(t: Track): string | null {
  const a = artistKey(t);
  if (!a) return null;
  return `${a}|${normalizeTitle(t.title)}`;
}

interface AutoplayCtx {
  autoplay: boolean;
  setAutoplay: (v: boolean) => void;
}

const Ctx = createContext<AutoplayCtx | null>(null);

export function AutoplayProvider({ children }: { children: ReactNode }) {
  const [autoplay, setAutoplayState] = useState<boolean>(() => {
    try {
      return localStorage.getItem(STORAGE_KEY) === "1";
    } catch {
      return false;
    }
  });
  const setAutoplay = useCallback((v: boolean) => {
    setAutoplayState(v);
    try {
      localStorage.setItem(STORAGE_KEY, v ? "1" : "0");
    } catch {
      /* localStorage may be unavailable (private mode); ignore */
    }
  }, []);

  const { state, pushTrack, trackMeta } = useSync();
  const { queue, now_playing_index } = state.playback;

  // Lock: true while a refill is in flight. We deliberately leave it
  // set for a short cooldown after the pushes resolve, see comment on
  // REFILL_COOLDOWN_MS.
  const isRefillingRef = useRef(false);

  useEffect(() => {
    if (!autoplay) return;
    if (now_playing_index === null) return;
    const items = queue.items;
    if (items.length === 0) return;

    const upcomingStart = now_playing_index + 1;
    const upcomingCount = items.length - upcomingStart;
    if (upcomingCount >= MIN_UPCOMING) return;
    if (isRefillingRef.current) return;

    // Seed candidates: walk back from the tail. The most-recent track
    // is the strongest signal of "what the user is in the mood for
    // right now"; falls back to earlier items if it isn't embedded yet.
    // startStationFromAny handles the not-embedded fallthrough internally.
    const seedCandidates: string[] = [];
    for (let j = items.length - 1; j >= 0; j--) {
      const id = items[j]?.track_id;
      if (id) seedCandidates.push(id);
    }
    if (seedCandidates.length === 0) return;

    // Local dedup state, all derived from the current queue snapshot:
    //   - queuedIds: exact track-id collisions (cheapest filter)
    //   - queuedKeys: (artist, normalized-title) — catches album/single
    //     versions of the same song
    //   - artistCounts: enforces the per-artist cap below
    // trackMeta is populated as we pushTrack, so it covers everything
    // autoplay has added; first-seed tracks pushed by the user before
    // mounting may be missing meta and will be skipped from the
    // artist-count denominator (treated as no signal).
    const queuedIds = new Set<string>(items.map((it) => it.track_id));
    const queuedKeys = new Set<string>();
    const artistCounts = new Map<string, number>();
    for (const it of items) {
      const meta = trackMeta.get(it.track_id);
      if (!meta) continue;
      const k = dedupeKey(meta);
      if (k) queuedKeys.add(k);
      const a = artistKey(meta);
      if (a) artistCounts.set(a, (artistCounts.get(a) ?? 0) + 1);
    }
    const need = MIN_UPCOMING - upcomingCount;

    isRefillingRef.current = true;
    let cancelled = false;
    void (async () => {
      let added = 0;
      try {
        const { tracks } = await startStationFromAny(
          seedCandidates,
          FETCH_BUFFER
        );
        if (cancelled) return;
        for (const t of tracks) {
          if (added >= need) break;
          if (queuedIds.has(t.id)) continue;
          const k = dedupeKey(t);
          if (k && queuedKeys.has(k)) continue;
          const a = artistKey(t);
          if (a && (artistCounts.get(a) ?? 0) >= MAX_PER_ARTIST) continue;
          pushTrack(t);
          queuedIds.add(t.id);
          if (k) queuedKeys.add(k);
          if (a) artistCounts.set(a, (artistCounts.get(a) ?? 0) + 1);
          added++;
        }
      } catch {
        // Silent — recommender unavailable, no embedded seed, etc.
        // The user-visible effect is "the queue stays under threshold",
        // which is the same fallback behaviour a non-autoplay queue
        // would have anyway. Surfacing this as a toast on every miss
        // would be noisy.
      } finally {
        // Cooldown to outlast the WS round-trip; see top-of-file note.
        // Release the lock unconditionally — `cancelled` is about not
        // pushing stale tracks, not about lock hygiene. Gating release
        // on `cancelled` would deadlock the lock on any non-trivial
        // refill, since cleanup fires on every queue.items broadcast
        // (5+ times during a single refill) and `cancelled` would
        // already be true by the time this timeout runs.
        //
        // Underdelivery → bump the cooldown. Means the diversity filters
        // ate most candidates; retrying immediately with the same seeds
        // would yield the same results. Hold off until the cursor moves
        // and the seed pool freshens.
        const cooldown =
          added < need ? UNDERDELIVERY_COOLDOWN_MS : REFILL_COOLDOWN_MS;
        setTimeout(() => {
          isRefillingRef.current = false;
        }, cooldown);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [autoplay, queue.items, now_playing_index, pushTrack, trackMeta]);

  return (
    <Ctx.Provider value={{ autoplay, setAutoplay }}>{children}</Ctx.Provider>
  );
}

export function useAutoplay(): AutoplayCtx {
  const v = useContext(Ctx);
  if (!v) throw new Error("useAutoplay must be used inside <AutoplayProvider>");
  return v;
}
