// /diagnostics/recommender — queue-fill / shortfall / similarity / top-results.
//
// Four sub-panels keyed to the /v1/diagnostics/recommend/* aggregations,
// all sharing a window selector. Layout: a top row of summary tiles +
// shortfall pills (immediate "is everything fine?" read), then the
// fill-ratio histogram, then similarity stats, then the top-results
// leaderboard.

import { useQuery } from "@tanstack/react-query";
import { useMemo, useState } from "react";

import {
  QueueFillResponse,
  ShortfallResponse,
  SimilarityResponse,
  TopResultsResponse,
  fetchRecommendQueueFill,
  fetchRecommendShortfall,
  fetchRecommendSimilarity,
  fetchRecommendTopResults,
} from "../../api/diagnostics";
import { Link } from "../../router";
import { DiagSection, ErrorLine, REFRESH_MS } from "./shared";

const TOP_RESULTS_LIMIT = 20;

const WINDOW_OPTIONS: ReadonlyArray<{ label: string; ms: number | null }> = [
  { label: "1h", ms: 60 * 60 * 1000 },
  { label: "24h", ms: 24 * 60 * 60 * 1000 },
  { label: "7d", ms: 7 * 24 * 60 * 60 * 1000 },
  { label: "30d", ms: 30 * 24 * 60 * 60 * 1000 },
  { label: "all", ms: null },
];

export function Recommender() {
  return (
    <div className="section">
      <div className="section-head">
        <h2>recommender</h2>
        <span className="count">refresh {REFRESH_MS / 1000}s</span>
      </div>

      <DiagSection title="recommender">
        <p className="text-sm" style={{ marginBottom: "var(--space-3)" }}>
          <Link to="/settings/latent">→ latent space (2-D UMAP scatter)</Link>
        </p>
        <RecommenderPanels />
      </DiagSection>
    </div>
  );
}

function RecommenderPanels() {
  // Default 24h: long enough to have data even on a quiet day, short
  // enough that p50/p95 reflect "what the recommender is doing right
  // now," not last month's average.
  const [windowIdx, setWindowIdx] = useState(1);
  const win = WINDOW_OPTIONS[windowIdx]!;
  // Memoize on win.ms — re-evaluating `Date.now()` every render would
  // invalidate the query keys and refetch on every paint.
  const sinceMs = useMemo(
    () => (win.ms == null ? undefined : Date.now() - win.ms),
    [win.ms]
  );
  const queryArg = sinceMs === undefined ? {} : { sinceMs };

  const fill = useQuery({
    queryKey: ["diag", "rec", "fill", win.label],
    queryFn: () => fetchRecommendQueueFill(queryArg),
    refetchInterval: REFRESH_MS,
  });
  const shortfall = useQuery({
    queryKey: ["diag", "rec", "shortfall", win.label],
    queryFn: () => fetchRecommendShortfall(queryArg),
    refetchInterval: REFRESH_MS,
  });
  const similarity = useQuery({
    queryKey: ["diag", "rec", "similarity", win.label],
    queryFn: () => fetchRecommendSimilarity(queryArg),
    refetchInterval: REFRESH_MS,
  });
  const topResults = useQuery({
    queryKey: ["diag", "rec", "top_results", win.label],
    queryFn: () =>
      fetchRecommendTopResults({ ...queryArg, limit: TOP_RESULTS_LIMIT }),
    refetchInterval: REFRESH_MS,
  });

  return (
    <div>
      <div className="flex items-center gap-2 mb-3">
        <label className="text-fg-muted text-sm">window:</label>
        <select
          className="search-input"
          style={{ width: "auto", paddingLeft: 12 }}
          value={windowIdx}
          onChange={(e) => setWindowIdx(Number(e.target.value))}
        >
          {WINDOW_OPTIONS.map((w, i) => (
            <option key={w.label} value={i}>
              {w.label}
            </option>
          ))}
        </select>
      </div>

      <h4 className="text-fg-muted text-sm mb-2">queue health</h4>
      {fill.error && <ErrorLine error={fill.error} />}
      {fill.data && <QueueHealthTiles data={fill.data} />}

      <h4 className="text-fg-muted text-sm mt-5 mb-2">fill ratio</h4>
      {fill.data && <FillHistogram data={fill.data} />}

      <h4 className="text-fg-muted text-sm mt-5 mb-2">shortfall reasons</h4>
      {shortfall.error && <ErrorLine error={shortfall.error} />}
      {shortfall.data && <ShortfallPills data={shortfall.data} />}

      <h4 className="text-fg-muted text-sm mt-5 mb-2">admitted similarity</h4>
      {similarity.error && <ErrorLine error={similarity.error} />}
      {similarity.data && <SimilarityTiles data={similarity.data} />}

      <h4 className="text-fg-muted text-sm mt-5 mb-2">
        most-recommended tracks (limit {TOP_RESULTS_LIMIT})
      </h4>
      {topResults.error && <ErrorLine error={topResults.error} />}
      {topResults.data && <TopResultsTable data={topResults.data} />}
    </div>
  );
}

function QueueHealthTiles({ data }: { data: QueueFillResponse }) {
  // Three numbers worth surfacing at a glance:
  // - total: how many recommend calls landed in the window (with R1 fields).
  // - full: count in the 100% bucket — the "healthy" outcome.
  // - fullRatio: percentage of calls that fully delivered.
  const full = data.buckets.find((b) => b.label === "100%")?.count ?? 0;
  const total = data.total;
  const fullRatio = total === 0 ? null : Math.round((full / total) * 100);
  const empty = data.buckets.find((b) => b.label === "0%")?.count ?? 0;
  const tiles = [
    { label: "calls", value: String(total), tone: "" },
    { label: "fully delivered", value: String(full), tone: "" },
    {
      label: "% full",
      value: fullRatio === null ? "—" : `${fullRatio}%`,
      tone:
        fullRatio === null
          ? ""
          : fullRatio >= 90
            ? "is-good"
            : fullRatio >= 60
              ? "is-warn"
              : "is-danger",
    },
    {
      label: "empty slates",
      value: String(empty),
      tone: empty > 0 ? "is-danger" : "",
    },
  ];
  return (
    <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
      {tiles.map((t) => (
        <div key={t.label} className={`tile-stat ${t.tone}`}>
          <span className="stat-label">{t.label}</span>
          <span className="stat-value">{t.value}</span>
        </div>
      ))}
    </div>
  );
}

function FillHistogram({ data }: { data: QueueFillResponse }) {
  if (data.total === 0) {
    return (
      <p className="text-fg-faint text-sm">
        no recommend calls with R1 fields in window (pre-R1 spans don't
        contribute).
      </p>
    );
  }
  const max = Math.max(...data.buckets.map((b) => b.count), 1);
  return (
    <div className="overflow-x-auto">
      <table className="tracks tabular" style={{ fontFamily: "var(--font-mono)" }}>
        <thead>
          <tr>
            <th>bucket</th>
            <th style={{ textAlign: "right" }}>count</th>
            <th style={{ width: "60%" }}>distribution</th>
          </tr>
        </thead>
        <tbody>
          {data.buckets.map((b) => {
            const pct = (b.count / max) * 100;
            // Color picks one of three tones: empty (danger), partial
            // (warn), full (good). Keeps the chart legible without a
            // legend.
            const tone =
              b.label === "100%"
                ? "var(--color-good, #22c55e)"
                : b.label === "0%"
                  ? "var(--color-danger, #ef4444)"
                  : "var(--color-warn, #eab308)";
            return (
              <tr key={b.label} style={{ cursor: "default" }}>
                <td className="col-title" style={{ fontFamily: "var(--font-mono)" }}>
                  {b.label}
                </td>
                <td className="col-time">{b.count}</td>
                <td>
                  <div
                    style={{
                      height: 12,
                      background:
                        "color-mix(in oklab, var(--surface-0) 70%, transparent)",
                      borderRadius: "var(--radius-1)",
                      overflow: "hidden",
                    }}
                    title={`${b.count} call${b.count === 1 ? "" : "s"}`}
                  >
                    <div
                      style={{
                        width: `${pct}%`,
                        height: "100%",
                        background: tone,
                        borderRadius: "var(--radius-1)",
                      }}
                    />
                  </div>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function ShortfallPills({ data }: { data: ShortfallResponse }) {
  const entries = Object.entries(data.counts).sort((a, b) => b[1] - a[1]);
  if (entries.length === 0) {
    return <p className="text-fg-faint text-sm">no recommend calls in window.</p>;
  }
  return (
    <div className="flex flex-wrap gap-2">
      {entries.map(([reason, count]) => {
        // Map the four enum variants (+ unknown) to a sensible color.
        // "none" is the success case → good; everything else is a
        // failure mode worth investigating, with magnitude conveyed by
        // count alone.
        const tone =
          reason === "none"
            ? "is-good"
            : reason === "unknown"
              ? ""
              : "is-warn";
        return (
          <span key={reason} className={`pill ${tone}`}>
            {reason} · {count}
          </span>
        );
      })}
    </div>
  );
}

function SimilarityTiles({ data }: { data: SimilarityResponse }) {
  if (data.count === 0) {
    return <p className="text-fg-faint text-sm">no admitted similarities in window.</p>;
  }
  // Format as 3 decimals: cosine sims live in [-1, 1] and most practical
  // signal is past the second decimal.
  const fmt = (v: number) => v.toFixed(3);
  const tiles = [
    { label: "count", value: String(data.count), tone: "" },
    { label: "p50", value: fmt(data.p50), tone: "" },
    { label: "p90", value: fmt(data.p90), tone: "" },
    { label: "p95", value: fmt(data.p95), tone: "" },
    { label: "p99", value: fmt(data.p99), tone: "" },
    { label: "mean", value: fmt(data.mean), tone: "" },
    { label: "min", value: fmt(data.min), tone: "" },
    { label: "max", value: fmt(data.max), tone: "" },
  ];
  return (
    <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
      {tiles.map((t) => (
        <div key={t.label} className={`tile-stat ${t.tone}`}>
          <span className="stat-label">{t.label}</span>
          <span className="stat-value">{t.value}</span>
        </div>
      ))}
    </div>
  );
}

function TopResultsTable({ data }: { data: TopResultsResponse }) {
  if (data.items.length === 0) {
    return (
      <p className="text-fg-faint text-sm">
        no recommend results recorded yet (pre-R1 spans don't contain track
        ids).
      </p>
    );
  }
  return (
    <div className="overflow-x-auto">
      <table className="tracks">
        <thead>
          <tr>
            <th className="col-time" style={{ textAlign: "right" }}>
              count
            </th>
            <th className="col-title">title</th>
            <th className="col-artist">artist</th>
            <th className="col-album">album</th>
          </tr>
        </thead>
        <tbody>
          {data.items.map((item) => (
            <tr key={item.track_id} style={{ cursor: "default" }}>
              <td className="col-time">{item.count}</td>
              <td className="col-title">
                {item.title ?? (
                  <span style={{ fontFamily: "var(--font-mono)" }}>
                    {item.track_id}
                  </span>
                )}
              </td>
              <td className="col-artist">{item.artist ?? "—"}</td>
              <td className="col-album">{item.album ?? "—"}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
