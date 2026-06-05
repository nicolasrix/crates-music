// "Downloads" — the web equivalent of the CLI's `music pinned` +
// `music cache stats` + `music cache evict`. Shows the two-budget usage
// (regular LRU vs pinned/never-evicted), the list of pinned tracks, and a
// "fit to budget" action. Read-through against the AudioCacheContext, which
// owns the IndexedDB store.

import { useQuery } from "@tanstack/react-query";
import { Download, HardDrive, Trash2 } from "lucide-react";

import { getSong } from "../api/client";
import { useAudioCache } from "../cache/AudioCacheContext";
import { formatBytes } from "../cache/format";
import { Layout } from "../components/Layout";
import { TrackTable } from "../components/TrackTable";
import { usePlayback } from "../sync/usePlayback";
import type { Track } from "../api/types";

function fulfilled<T>(settled: PromiseSettledResult<T>[]): T[] {
  return settled
    .filter((s): s is PromiseFulfilledResult<T> => s.status === "fulfilled")
    .map((s) => s.value);
}

function UsageBar({
  label,
  used,
  budget,
  count,
}: {
  label: string;
  used: number;
  budget: number;
  count: number;
}) {
  const pct = budget > 0 ? Math.min(100, (used / budget) * 100) : 0;
  const over = used > budget;
  return (
    <div style={{ marginBottom: 16 }}>
      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: "baseline",
          marginBottom: 4,
        }}
      >
        <span style={{ fontWeight: 500 }}>{label}</span>
        <span className="text-fg-muted text-sm" style={{ fontVariantNumeric: "tabular-nums" }}>
          {formatBytes(used)} / {formatBytes(budget)} · {count} track{count === 1 ? "" : "s"}
        </span>
      </div>
      <div
        style={{
          height: 8,
          borderRadius: 4,
          background: "var(--bg-elevated)",
          border: "1px solid var(--border)",
          overflow: "hidden",
        }}
      >
        <div
          style={{
            height: "100%",
            width: `${pct}%`,
            background: over ? "var(--danger, #e0533d)" : "var(--accent, #f0a020)",
            transition: "width 200ms",
          }}
        />
      </div>
    </div>
  );
}

export function Downloads() {
  const cache = useAudioCache();
  const { playList } = usePlayback();

  const statsQ = useQuery({
    // revision bumps on every cache mutation → re-read.
    queryKey: ["cache", "stats", cache.revision],
    queryFn: () => cache.stats(),
  });

  const pinnedQ = useQuery({
    queryKey: ["cache", "pinned-hydrated", cache.revision],
    queryFn: async () => {
      const entries = await cache.listPinned();
      const tracks = await Promise.allSettled(entries.map((e) => getSong(e.trackId))).then(
        fulfilled,
      );
      return tracks;
    },
  });

  const s = statsQ.data;
  const tracks: Track[] = pinnedQ.data ?? [];

  return (
    <Layout breadcrumb="downloads">
      <div className="section">
        <div className="section-head">
          <h2>
            <HardDrive
              size={18}
              strokeWidth={1.5}
              style={{ verticalAlign: "-3px", marginRight: 8 }}
            />
            downloads &amp; cache
          </h2>
          <button
            type="button"
            className="text-sm"
            onClick={() => void cache.evictToBudget()}
            style={{
              display: "inline-flex",
              alignItems: "center",
              gap: 6,
              padding: "6px 10px",
              background: "var(--bg-elevated)",
              border: "1px solid var(--border)",
              borderRadius: "var(--radius-1, 2px)",
              color: "var(--fg)",
              cursor: "pointer",
            }}
          >
            <Trash2 size={14} strokeWidth={1.5} />
            free up space
          </button>
        </div>
        <p className="lead">
          Played tracks are cached automatically and evicted oldest-first.
          Tracks you save for offline are kept in a separate budget and never
          auto-evicted. Tune both budgets in Settings.
        </p>

        {s && (
          <div style={{ marginTop: 12 }}>
            <UsageBar
              label="Pinned (offline downloads)"
              used={s.pinnedBytes}
              budget={s.pinnedBudgetBytes}
              count={s.pinnedCount}
            />
            <UsageBar
              label="Recent (auto-cached)"
              used={s.regularBytes}
              budget={s.regularBudgetBytes}
              count={s.regularCount}
            />
            {s.storageQuotaBytes !== undefined && s.storageUsedBytes !== undefined && (
              <p className="text-fg-muted text-sm" style={{ marginTop: 4 }}>
                Browser storage: {formatBytes(s.storageUsedBytes)} used of{" "}
                {formatBytes(s.storageQuotaBytes)} available
                {s.persisted ? " · persistent" : " · best-effort (not persisted)"}.
              </p>
            )}
          </div>
        )}
      </div>

      <div className="section">
        <div className="section-head">
          <h2>
            <Download
              size={16}
              strokeWidth={1.5}
              style={{ verticalAlign: "-2px", marginRight: 8 }}
            />
            saved for offline
          </h2>
          {tracks.length > 0 && <span className="count tabular">{tracks.length}</span>}
        </div>
        {pinnedQ.isLoading && <p className="text-fg-muted text-sm">loading…</p>}
        {!pinnedQ.isLoading && tracks.length === 0 && (
          <p className="text-fg-muted text-sm">
            nothing saved yet — open a track's ⋯ menu and choose “save for
            offline”.
          </p>
        )}
        {tracks.length > 0 && (
          <TrackTable tracks={tracks} showAlbum onPlay={(i) => playList(tracks, i)} />
        )}
      </div>
    </Layout>
  );
}
