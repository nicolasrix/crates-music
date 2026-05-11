// Data-dense diagnostics page — mirrors the schema in api/diagnostics.ts
// exactly. Four sections: ingest queue tiles, span histogram with inline
// 3-segment box-bar (p99 danger / p95 warning / p50 success layered, plus
// a 1-px max marker), client-events RUM, and recent traces grouped by
// trace_id with an expandable waterfall.
//
// 5-second TanStack Query refetchInterval keeps the page "live enough"
// for a human watching ingest progress without hammering the gateway.

import { useQuery } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import {
  ClientEventEntry,
  HistogramBucket,
  QueueFillResponse,
  RecentlyPlayedEntry,
  ShortfallResponse,
  SimilarityResponse,
  TopResultsResponse,
  TraceEntry,
  fetchClientEvents,
  fetchHistogram,
  fetchQueueDepth,
  fetchRecentlyPlayed,
  fetchRecommendQueueFill,
  fetchRecommendShortfall,
  fetchRecommendSimilarity,
  fetchRecommendTopResults,
  fetchTraces,
} from "../api/diagnostics";
import { Layout } from "../components/Layout";
import { Link } from "../router";
import { fmtMs, fmtRelativePast } from "../utils/format";

const REFRESH_MS = 5_000;
const TRACES_LIMIT = 200;

export function Diagnostics() {
  const queue = useQuery({
    queryKey: ["diag", "queue_depth"],
    queryFn: () => fetchQueueDepth({}),
    refetchInterval: REFRESH_MS,
  });
  const histogram = useQuery({
    queryKey: ["diag", "histogram"],
    queryFn: () => fetchHistogram({}),
    refetchInterval: REFRESH_MS,
  });
  const [nameFilter, setNameFilter] = useState<string>("");
  const traces = useQuery({
    queryKey: ["diag", "traces", nameFilter],
    queryFn: () =>
      fetchTraces(
        nameFilter
          ? { limit: TRACES_LIMIT, name: nameFilter }
          : { limit: TRACES_LIMIT }
      ),
    refetchInterval: REFRESH_MS,
  });

  return (
    <Layout breadcrumb="diagnostics">
      <div className="section">
        <div className="section-head">
          <h2>diagnostics</h2>
          <span className="count">refresh {REFRESH_MS / 1000}s</span>
        </div>

        <DiagSection title="ingest queue">
          {queue.error && <ErrorLine error={queue.error} />}
          {queue.data && <QueueDepth data={queue.data} />}
        </DiagSection>

        <DiagSection title="recommender">
          <p className="text-sm" style={{ marginBottom: "var(--space-3)" }}>
            <Link to="/diagnostics/latent">→ latent space (2-D UMAP scatter)</Link>
          </p>
          <RecommenderSection />
        </DiagSection>

        <DiagSection title="span duration histogram">
          {histogram.error && <ErrorLine error={histogram.error} />}
          {histogram.data && <HistogramTable buckets={histogram.data.buckets} />}
        </DiagSection>

        <DiagSection title="client events (RUM)">
          <ClientEventsSection />
        </DiagSection>

        <DiagSection title="recently played">
          <RecentlyPlayedSection />
        </DiagSection>

        <DiagSection title="recent traces">
          <div className="flex items-center gap-2 mb-3">
            <label className="text-fg-muted text-sm">filter by name:</label>
            <select
              className="search-input"
              style={{ width: "auto", paddingLeft: 12 }}
              value={nameFilter}
              onChange={(e) => setNameFilter(e.target.value)}
            >
              <option value="">(all)</option>
              {(histogram.data?.buckets ?? []).map((b) => (
                <option key={b.name} value={b.name}>
                  {b.name}
                </option>
              ))}
            </select>
            <span className="text-fg-faint text-xs">limit {TRACES_LIMIT}</span>
          </div>
          {traces.error && <ErrorLine error={traces.error} />}
          {traces.data && <TraceList entries={traces.data.traces} />}
        </DiagSection>
      </div>
    </Layout>
  );
}

function DiagSection({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section style={{ marginBottom: "var(--space-7)" }}>
      <h3 className="text-lg font-medium mb-3">{title}</h3>
      {children}
    </section>
  );
}

function ErrorLine({ error }: { error: unknown }) {
  return <p className="text-danger text-sm">error: {(error as Error).message}</p>;
}

// --- queue depth -----------------------------------------------------------

function QueueDepth({
  data,
}: {
  data: { model_version: string; not_started: number; in_progress: number; done: number; failed: number };
}) {
  const tiles = [
    { label: "not started", value: data.not_started, tone: "" },
    {
      label: "in progress",
      value: data.in_progress,
      tone: data.in_progress > 0 ? "is-warn" : "",
    },
    { label: "done", value: data.done, tone: "" },
    {
      label: "failed",
      value: data.failed,
      tone: data.failed > 0 ? "is-danger" : "",
    },
  ];
  return (
    <div>
      <p className="text-fg-faint text-xs mb-2">model: {data.model_version}</p>
      <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
        {tiles.map((t) => (
          <div key={t.label} className={`tile-stat ${t.tone}`}>
            <span className="stat-label">{t.label}</span>
            <span className="stat-value">{t.value}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

// --- histogram -------------------------------------------------------------

function HistogramTable({ buckets }: { buckets: HistogramBucket[] }) {
  if (buckets.length === 0) {
    return <p className="text-fg-faint text-sm">no spans recorded yet.</p>;
  }
  // Scale all bars against the widest span across all buckets — makes a
  // p99=8000ms span visibly tower over a p99=1ms one. Per-row scaling would
  // mask the difference.
  const maxMs = Math.max(...buckets.map((b) => b.max_ms), 1);
  return (
    <div className="overflow-x-auto">
      <table className="tracks tabular" style={{ fontFamily: "var(--font-mono)" }}>
        <thead>
          <tr>
            <th>name</th>
            <th style={{ textAlign: "right" }}>count</th>
            <th style={{ textAlign: "right" }}>min</th>
            <th style={{ textAlign: "right" }}>p50</th>
            <th style={{ textAlign: "right" }}>p95</th>
            <th style={{ textAlign: "right" }}>p99</th>
            <th style={{ textAlign: "right" }}>max</th>
            <th style={{ width: "30%" }}>distribution</th>
          </tr>
        </thead>
        <tbody>
          {buckets.map((b) => (
            <tr key={b.name} style={{ cursor: "default" }}>
              <td className="col-title" style={{ fontFamily: "var(--font-mono)" }}>
                {b.name}
              </td>
              <td className="col-time">{b.count}</td>
              <td className="col-time">{fmtMs(b.min_ms)}</td>
              <td className="col-time">{fmtMs(b.p50_ms)}</td>
              <td className="col-time">{fmtMs(b.p95_ms)}</td>
              <td className="col-time">{fmtMs(b.p99_ms)}</td>
              <td className="col-time">{fmtMs(b.max_ms)}</td>
              <td>
                <BoxBar
                  p50={b.p50_ms}
                  p95={b.p95_ms}
                  p99={b.p99_ms}
                  max={b.max_ms}
                  scaleMax={maxMs}
                />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function BoxBar({
  p50,
  p95,
  p99,
  max,
  scaleMax,
}: {
  p50: number;
  p95: number;
  p99: number;
  max: number;
  scaleMax: number;
}) {
  const pct = (v: number) => `${Math.max(0, Math.min(100, (v / scaleMax) * 100))}%`;
  return (
    <div className="hist-bar" title={`max ${fmtMs(max)}`}>
      <div className="seg p99" style={{ width: pct(p99) }} />
      <div className="seg p95" style={{ width: pct(p95) }} />
      <div className="seg p50" style={{ width: pct(p50) }} />
      <div className="marker-max" style={{ left: pct(max) }} />
    </div>
  );
}

// --- traces ----------------------------------------------------------------

interface Group {
  trace_id: string;
  start_ms: number;
  end_ms: number;
  spans: TraceEntry[];
}

function groupByTrace(entries: TraceEntry[]): Group[] {
  const map = new Map<string, Group>();
  for (const e of entries) {
    let g = map.get(e.trace_id);
    if (!g) {
      g = { trace_id: e.trace_id, start_ms: e.start_ms, end_ms: e.end_ms, spans: [] };
      map.set(e.trace_id, g);
    }
    g.spans.push(e);
    if (e.start_ms < g.start_ms) g.start_ms = e.start_ms;
    if (e.end_ms > g.end_ms) g.end_ms = e.end_ms;
  }
  return [...map.values()].sort((a, b) => b.end_ms - a.end_ms);
}

function TraceList({ entries }: { entries: TraceEntry[] }) {
  const groups = useMemo(() => groupByTrace(entries), [entries]);
  if (groups.length === 0) {
    return <p className="text-fg-faint text-sm">no traces in window.</p>;
  }
  return (
    <div className="flex flex-col gap-3">
      {groups.map((g) => (
        <TraceCard key={g.trace_id} group={g} />
      ))}
    </div>
  );
}

function TraceCard({ group }: { group: Group }) {
  const [open, setOpen] = useState(false);
  const totalMs = Math.max(group.end_ms - group.start_ms, 1);
  const sorted = [...group.spans].sort((a, b) => a.start_ms - b.start_ms);
  return (
    <div
      style={{
        border: "1px solid var(--border-subtle)",
        borderRadius: "var(--radius-2)",
        background: "color-mix(in oklab, var(--surface-1) 60%, transparent)",
      }}
    >
      <button
        className="w-full flex items-center justify-between px-3 py-2 text-left"
        onClick={() => setOpen((v) => !v)}
        style={{ background: "transparent", border: 0, cursor: "pointer", color: "inherit" }}
      >
        <span className="font-mono text-fg-muted text-xs">{group.trace_id}</span>
        <span className="text-fg-faint text-xs">
          {sorted.length} span{sorted.length === 1 ? "" : "s"} · {fmtMs(totalMs)}
        </span>
      </button>
      <div className="px-3 pb-3 flex flex-col gap-1">
        {sorted.map((s) => (
          <SpanBar
            key={s.span_id}
            span={s}
            traceStart={group.start_ms}
            totalMs={totalMs}
            expanded={open}
          />
        ))}
      </div>
    </div>
  );
}

function SpanBar({
  span,
  traceStart,
  totalMs,
  expanded,
}: {
  span: TraceEntry;
  traceStart: number;
  totalMs: number;
  expanded: boolean;
}) {
  const offsetPct = ((span.start_ms - traceStart) / totalMs) * 100;
  const widthPct = Math.max((span.duration_ms / totalMs) * 100, 0.3);
  const color = nameToHsl(span.name);
  return (
    <div>
      <div className="flex items-center gap-2 text-xs">
        <span
          className="font-mono text-fg-muted truncate"
          style={{ width: 224 }}
          title={span.name}
        >
          {span.name}
        </span>
        <div
          className="flex-1 relative"
          style={{
            height: 16,
            background: "color-mix(in oklab, var(--surface-0) 70%, transparent)",
            borderRadius: "var(--radius-1)",
            overflow: "hidden",
          }}
        >
          <div
            className="absolute top-0 bottom-0"
            style={{
              left: `${offsetPct}%`,
              width: `${widthPct}%`,
              backgroundColor: color,
              borderRadius: "var(--radius-1)",
            }}
            title={`${span.name}: ${fmtMs(span.duration_ms)}`}
          />
        </div>
        <span
          className="tabular text-fg-muted text-right"
          style={{ width: 64, fontFamily: "var(--font-mono)" }}
        >
          {fmtMs(span.duration_ms)}
        </span>
      </div>
      {expanded && Object.keys(span.fields).length > 0 && (
        <pre
          className="text-fg-faint mt-1 px-2 py-1 overflow-x-auto"
          style={{
            marginLeft: 224,
            fontSize: 11,
            background: "color-mix(in oklab, var(--surface-0) 70%, transparent)",
            borderRadius: "var(--radius-1)",
          }}
        >
          {JSON.stringify(span.fields, null, 0)}
        </pre>
      )}
    </div>
  );
}

// --- client events ---------------------------------------------------------

function ClientEventsSection() {
  const events = useQuery({
    queryKey: ["diag", "client_events"],
    queryFn: () => fetchClientEvents({ limit: 100 }),
    refetchInterval: REFRESH_MS,
  });
  if (events.error) return <ErrorLine error={events.error} />;
  if (!events.data) return null;
  if (events.data.events.length === 0) {
    return <p className="text-fg-faint text-sm">no client events yet.</p>;
  }
  return (
    <div className="overflow-x-auto">
      <table className="tracks tabular" style={{ fontFamily: "var(--font-mono)" }}>
        <thead>
          <tr>
            <th>received</th>
            <th>name</th>
            <th style={{ textAlign: "right" }}>value</th>
            <th>rating</th>
            <th>page</th>
            <th>session</th>
          </tr>
        </thead>
        <tbody>
          {events.data.events.map((e, i) => (
            <ClientEventRow key={`${e.session_id}-${e.received_ms}-${i}`} event={e} />
          ))}
        </tbody>
      </table>
    </div>
  );
}

function ClientEventRow({ event }: { event: ClientEventEntry }) {
  return (
    <tr style={{ cursor: "default" }}>
      <td className="col-time" style={{ textAlign: "left", fontFamily: "var(--font-mono)" }}>
        {fmtRecentTime(event.received_ms)}
      </td>
      <td className="col-title" style={{ fontFamily: "var(--font-mono)" }}>
        {event.name}
      </td>
      <td className="col-time">
        {event.value_ms === null ? "—" : fmtMs(event.value_ms)}
      </td>
      <td>{event.rating ? <RatingPill rating={event.rating} /> : "—"}</td>
      <td className="col-artist" style={{ fontFamily: "var(--font-mono)" }}>
        {event.page_path}
      </td>
      <td className="col-artist" style={{ fontFamily: "var(--font-mono)" }}>
        {event.session_id.slice(0, 8)}
      </td>
    </tr>
  );
}

function RatingPill({ rating }: { rating: "good" | "needs-improvement" | "poor" }) {
  const cls = rating === "good" ? "is-good" : rating === "poor" ? "is-poor" : "is-warn";
  return <span className={`pill ${cls}`}>{rating}</span>;
}

function fmtRecentTime(unixMs: number): string {
  const d = new Date(unixMs);
  return d.toLocaleTimeString();
}

// Stable color per span name for the waterfall. djb2 hash → HSL.
function nameToHsl(name: string): string {
  let h = 5381;
  for (let i = 0; i < name.length; i++) {
    h = ((h << 5) + h + name.charCodeAt(i)) | 0;
  }
  const hue = ((h % 360) + 360) % 360;
  return `hsl(${hue}, 60%, 55%)`;
}

// --- recommender ----------------------------------------------------------
//
// Four sub-panels keyed to the /v1/diagnostics/recommend/* aggregations,
// all sharing a window selector. Layout: a top row of summary tiles +
// shortfall pills (immediate "is everything fine?" read), then the
// fill-ratio histogram, then similarity stats, then the top-results
// leaderboard.

const RECOMMEND_WINDOW_OPTIONS: ReadonlyArray<{ label: string; ms: number | null }> = [
  { label: "1h", ms: 60 * 60 * 1000 },
  { label: "24h", ms: 24 * 60 * 60 * 1000 },
  { label: "7d", ms: 7 * 24 * 60 * 60 * 1000 },
  { label: "30d", ms: 30 * 24 * 60 * 60 * 1000 },
  { label: "all", ms: null },
];

const TOP_RESULTS_LIMIT = 20;

function RecommenderSection() {
  // Default 24h: long enough to have data even on a quiet day, short
  // enough that p50/p95 reflect "what the recommender is doing right
  // now," not last month's average.
  const [windowIdx, setWindowIdx] = useState(1);
  const win = RECOMMEND_WINDOW_OPTIONS[windowIdx]!;
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
          {RECOMMEND_WINDOW_OPTIONS.map((w, i) => (
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
  // signal is past the second decimal. The fmtSim helper rounds without
  // padding so 1.0 doesn't render as "1.000".
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

// --- recently played ------------------------------------------------------
//
// Why a window selector + repeat count: B1 needs to pick a recency-window
// default for MMR's "don't surface the same track twice" penalty. The
// signal that matters is "how many tracks repeat inside window W?" — if
// a 24h window has lots of repeats, MMR shouldn't bother penalising.
// If a 7-day window still has noticeable repeats, that's where the
// penalty kicks in.

const RECENT_LIMIT = 200;

const WINDOW_OPTIONS: ReadonlyArray<{ label: string; ms: number | null }> = [
  { label: "1h", ms: 60 * 60 * 1000 },
  { label: "24h", ms: 24 * 60 * 60 * 1000 },
  { label: "7d", ms: 7 * 24 * 60 * 60 * 1000 },
  { label: "30d", ms: 30 * 24 * 60 * 60 * 1000 },
  { label: "all", ms: null },
];

function RecentlyPlayedSection() {
  const [windowIdx, setWindowIdx] = useState(2); // default 7d
  const win = WINDOW_OPTIONS[windowIdx]!;
  // Recompute since_ms only when the window changes — avoids invalidating
  // the query on every render due to a fresh Date.now().
  const sinceMs = useMemo(
    () => (win.ms == null ? undefined : Date.now() - win.ms),
    [win.ms]
  );
  const recently = useQuery({
    queryKey: ["diag", "recently_played", win.label],
    queryFn: () =>
      fetchRecentlyPlayed(
        sinceMs === undefined
          ? { limit: RECENT_LIMIT }
          : { limit: RECENT_LIMIT, sinceMs }
      ),
    refetchInterval: REFRESH_MS,
  });

  if (recently.error) return <ErrorLine error={recently.error} />;
  const events = recently.data?.events ?? [];

  const uniqueTracks = new Set(events.map((e) => e.track_id)).size;
  const repeats = events.length - uniqueTracks;

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
        <span className="text-fg-faint text-xs">
          limit {RECENT_LIMIT} · {events.length} events · {uniqueTracks} unique ·{" "}
          {repeats} repeats
        </span>
      </div>
      {events.length === 0 ? (
        <p className="text-fg-faint text-sm">no scrobbles in window.</p>
      ) : (
        <RecentlyPlayedTable events={events} />
      )}
    </div>
  );
}

function RecentlyPlayedTable({ events }: { events: RecentlyPlayedEntry[] }) {
  // Per-track repeat counter so the table can flag "you played this 3 times
  // in this window" — the eyeball signal for window-size choice.
  const repeatCount = useMemo(() => {
    const counts = new Map<string, number>();
    for (const e of events) counts.set(e.track_id, (counts.get(e.track_id) ?? 0) + 1);
    return counts;
  }, [events]);
  return (
    <div className="overflow-x-auto">
      <table className="tracks">
        <thead>
          <tr>
            <th className="col-time" style={{ textAlign: "left" }}>
              when
            </th>
            <th className="col-title">title</th>
            <th className="col-artist">artist</th>
            <th className="col-album">album</th>
            <th className="col-time" style={{ textAlign: "right" }}>
              repeats
            </th>
          </tr>
        </thead>
        <tbody>
          {events.map((e) => {
            const repeats = repeatCount.get(e.track_id) ?? 1;
            // Album column collapses album name + year into one cell so
            // we don't grow the table to six columns. "Music for
            // Airports · 1978" is the format; either half may be
            // missing.
            const albumLabel = e.album
              ? e.year != null
                ? `${e.album} · ${e.year}`
                : e.album
              : null;
            return (
              <tr
                key={`${e.track_id}-${e.occurred_at_ms}-${e.received_at_ms}`}
                style={{ cursor: "default" }}
              >
                <td className="col-time" style={{ textAlign: "left" }}>
                  {fmtRelativePast(new Date(e.occurred_at_ms).toISOString()) ??
                    fmtRecentTime(e.occurred_at_ms)}
                </td>
                <td className="col-title">
                  {e.title ?? (
                    <span style={{ fontFamily: "var(--font-mono)" }}>{e.track_id}</span>
                  )}
                </td>
                <td className="col-artist">
                  {e.artist_id && e.artist ? (
                    <Link to={`/artists/${e.artist_id}`}>{e.artist}</Link>
                  ) : (
                    (e.artist ?? "—")
                  )}
                </td>
                <td className="col-album">
                  {e.album_id && albumLabel ? (
                    <Link to={`/albums/${e.album_id}`}>{albumLabel}</Link>
                  ) : (
                    (albumLabel ?? "—")
                  )}
                </td>
                <td className="col-time">{repeats > 1 ? `×${repeats}` : "—"}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
