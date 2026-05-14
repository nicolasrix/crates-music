// /diagnostics/tracing — three concentric levels of detail.
//
//   1. time window     (range selector at the page header)
//   2. span name       (aggregate histogram; one row "selected" at a time)
//   3. trace instance  (recent-traces list, auto-filtered to the selection)
//
// One time range and one selected name are shared across every panel on
// the page, so the histogram p50/p95/p99 and the scatter's reference
// lines refer to the same window, and clicking a histogram row narrows
// the trace list below — no separate name dropdown.

import { useQuery } from "@tanstack/react-query";
import { useMemo, useState } from "react";

import {
  HistogramBucket,
  SpanChildrenResponse,
  SpanSeriesPoint,
  TraceEntry,
  fetchHistogram,
  fetchSpanChildren,
  fetchSpanSeries,
  fetchTraces,
} from "../../api/diagnostics";
import { Layout } from "../../components/Layout";
import { fmtMs } from "../../utils/format";
import { DiagSection, ErrorLine, REFRESH_MS } from "./shared";

const TRACES_LIMIT = 200;
const SERIES_LIMIT = 5_000;

interface RangeOption {
  label: string;
  /** Milliseconds back from "now". `null` = no since filter (whole ring). */
  sinceMs: number | null;
}

const RANGES: ReadonlyArray<RangeOption> = [
  { label: "15m", sinceMs: 15 * 60_000 },
  { label: "1h", sinceMs: 60 * 60_000 },
  { label: "6h", sinceMs: 6 * 60 * 60_000 },
  { label: "24h", sinceMs: 24 * 60 * 60_000 },
  { label: "7d", sinceMs: 7 * 24 * 60 * 60_000 },
  { label: "all", sinceMs: null },
];

export function Tracing() {
  const [rangeIdx, setRangeIdx] = useState<number>(3); // default 24h
  const [selectedName, setSelectedName] = useState<string | null>(null);

  const range = RANGES[rangeIdx] ?? RANGES[RANGES.length - 1]!;
  // Re-evaluate `Date.now()` only when the user changes the range — the
  // refetchInterval handles fresh data within a window.
  const sinceMs = useMemo<number | undefined>(
    () => (range.sinceMs == null ? undefined : Date.now() - range.sinceMs),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [rangeIdx],
  );

  const histogram = useQuery({
    queryKey: ["diag", "histogram", rangeIdx],
    queryFn: () => fetchHistogram(sinceMs === undefined ? {} : { sinceMs }),
    refetchInterval: REFRESH_MS,
  });

  const traces = useQuery({
    queryKey: ["diag", "traces", rangeIdx, selectedName ?? ""],
    queryFn: () => {
      const base: { limit: number; sinceMs?: number; name?: string } = {
        limit: TRACES_LIMIT,
      };
      if (sinceMs !== undefined) base.sinceMs = sinceMs;
      if (selectedName) base.name = selectedName;
      return fetchTraces(base);
    },
    refetchInterval: REFRESH_MS,
  });

  return (
    <Layout breadcrumb="diagnostics / tracing">
      <div className="section">
        <div className="section-head">
          <h2>tracing</h2>
          <span className="count">refresh {REFRESH_MS / 1000}s</span>
        </div>

        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: "var(--space-2)",
            marginBottom: "var(--space-3)",
            flexWrap: "wrap",
          }}
        >
          <span className="text-fg-muted text-xs">time window:</span>
          <RangeChips rangeIdx={rangeIdx} onChange={setRangeIdx} />
          {selectedName && (
            <>
              <span className="text-fg-faint text-xs" style={{ marginLeft: 12 }}>
                selected:
              </span>
              <span
                className="text-xs"
                style={{
                  fontFamily: "var(--font-mono)",
                  padding: "2px 8px",
                  borderRadius: "var(--radius-1)",
                  border: "1px solid var(--border-subtle)",
                  background: "color-mix(in oklab, var(--accent) 18%, transparent)",
                }}
              >
                {selectedName}
              </span>
              <button
                type="button"
                onClick={() => setSelectedName(null)}
                className="text-xs"
                style={{
                  padding: "2px 8px",
                  borderRadius: "var(--radius-1)",
                  border: "1px solid var(--border-subtle)",
                  background: "transparent",
                  cursor: "pointer",
                  color: "inherit",
                  fontFamily: "var(--font-mono)",
                }}
              >
                clear
              </button>
            </>
          )}
        </div>

        <DiagSection title="1 · span aggregates — click a row to drill in">
          {histogram.error && <ErrorLine error={histogram.error} />}
          {histogram.data && (
            <HistogramTable
              buckets={histogram.data.buckets}
              selectedName={selectedName}
              onToggle={(name) =>
                setSelectedName(selectedName === name ? null : name)
              }
              sinceMs={sinceMs}
            />
          )}
        </DiagSection>

        <DiagSection
          title={
            selectedName
              ? `2 · recent traces — filtered to ${selectedName}`
              : "2 · recent traces — all spans in window"
          }
        >
          <div className="text-fg-faint text-xs" style={{ marginBottom: 8 }}>
            limit {TRACES_LIMIT} · {selectedName
              ? "click a row in the table above to change filter"
              : "click a histogram row above to filter"}
          </div>
          {traces.error && <ErrorLine error={traces.error} />}
          {traces.data && <TraceList entries={traces.data.traces} />}
        </DiagSection>
      </div>
    </Layout>
  );
}

function RangeChips({
  rangeIdx,
  onChange,
}: {
  rangeIdx: number;
  onChange: (i: number) => void;
}) {
  return (
    <div style={{ display: "flex", gap: 4 }}>
      {RANGES.map((r, i) => (
        <button
          key={r.label}
          type="button"
          onClick={() => onChange(i)}
          className="text-xs"
          style={{
            padding: "2px 8px",
            borderRadius: "var(--radius-1)",
            border: "1px solid var(--border-subtle)",
            background:
              i === rangeIdx
                ? "var(--accent)"
                : "color-mix(in oklab, var(--surface-1) 60%, transparent)",
            color: i === rangeIdx ? "var(--on-accent)" : "inherit",
            cursor: "pointer",
            fontFamily: "var(--font-mono)",
          }}
        >
          {r.label}
        </button>
      ))}
    </div>
  );
}

function HistogramTable({
  buckets,
  selectedName,
  onToggle,
  sinceMs,
}: {
  buckets: HistogramBucket[];
  selectedName: string | null;
  onToggle: (name: string) => void;
  sinceMs: number | undefined;
}) {
  if (buckets.length === 0) {
    return <p className="text-fg-faint text-sm">no spans recorded in window.</p>;
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
            <th style={{ width: 24 }} />
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
            <HistogramRow
              key={b.name}
              bucket={b}
              scaleMax={maxMs}
              open={selectedName === b.name}
              onToggle={() => onToggle(b.name)}
              sinceMs={sinceMs}
            />
          ))}
        </tbody>
      </table>
    </div>
  );
}

function HistogramRow({
  bucket,
  scaleMax,
  open,
  onToggle,
  sinceMs,
}: {
  bucket: HistogramBucket;
  scaleMax: number;
  open: boolean;
  onToggle: () => void;
  sinceMs: number | undefined;
}) {
  return (
    <>
      <tr
        onClick={onToggle}
        style={{ cursor: "pointer" }}
        title={open ? "click to deselect" : "click to drill in"}
      >
        <td
          className="text-fg-faint"
          style={{
            textAlign: "center",
            fontFamily: "var(--font-mono)",
            userSelect: "none",
          }}
        >
          {open ? "▾" : "▸"}
        </td>
        <td className="col-title" style={{ fontFamily: "var(--font-mono)" }}>
          {bucket.name}
        </td>
        <td className="col-time">{bucket.count}</td>
        <td className="col-time">{fmtMs(bucket.min_ms)}</td>
        <td className="col-time">{fmtMs(bucket.p50_ms)}</td>
        <td className="col-time">{fmtMs(bucket.p95_ms)}</td>
        <td className="col-time">{fmtMs(bucket.p99_ms)}</td>
        <td className="col-time">{fmtMs(bucket.max_ms)}</td>
        <td>
          <BoxBar
            p50={bucket.p50_ms}
            p95={bucket.p95_ms}
            p99={bucket.p99_ms}
            max={bucket.max_ms}
            scaleMax={scaleMax}
          />
        </td>
      </tr>
      {open && (
        <tr>
          <td
            colSpan={9}
            style={{
              padding: "var(--space-3) var(--space-4)",
              background:
                "color-mix(in oklab, var(--surface-0) 60%, transparent)",
            }}
          >
            <SpanDetailPanel bucket={bucket} sinceMs={sinceMs} />
          </td>
        </tr>
      )}
    </>
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
  const pctW = (v: number) =>
    `${Math.max(0, Math.min(100, (v / scaleMax) * 100))}%`;
  return (
    <div className="hist-bar" title={`max ${fmtMs(max)}`}>
      <div className="seg p99" style={{ width: pctW(p99) }} />
      <div className="seg p95" style={{ width: pctW(p95) }} />
      <div className="seg p50" style={{ width: pctW(p50) }} />
      <div className="marker-max" style={{ left: pctW(max) }} />
    </div>
  );
}

// --- per-name drill-down: scatter over time + child breakdown ----------

function SpanDetailPanel({
  bucket,
  sinceMs,
}: {
  bucket: HistogramBucket;
  sinceMs: number | undefined;
}) {
  const series = useQuery({
    queryKey: ["diag", "span_series", bucket.name, sinceMs ?? "all"],
    queryFn: () =>
      fetchSpanSeries(
        sinceMs === undefined
          ? { name: bucket.name, limit: SERIES_LIMIT }
          : { name: bucket.name, sinceMs, limit: SERIES_LIMIT },
      ),
    refetchInterval: REFRESH_MS,
  });

  const children = useQuery({
    queryKey: ["diag", "span_children", bucket.name, sinceMs ?? "all"],
    queryFn: () =>
      fetchSpanChildren(
        sinceMs === undefined
          ? { name: bucket.name }
          : { name: bucket.name, sinceMs },
      ),
    refetchInterval: REFRESH_MS,
  });

  return (
    <div className="flex flex-col gap-3">
      <div className="text-fg-faint text-xs">
        mean {fmtMs(bucket.mean_ms)} · samples in window:{" "}
        {series.data?.points.length ?? "…"}
      </div>

      {series.error && <ErrorLine error={series.error} />}
      {series.data && (
        <SpanScatter
          points={series.data.points}
          p50={bucket.p50_ms}
          p95={bucket.p95_ms}
          p99={bucket.p99_ms}
          max={bucket.max_ms}
        />
      )}

      {children.data && children.data.children.length > 0 && (
        <ChildBreakdownPanel data={children.data} />
      )}
    </div>
  );
}

function ChildBreakdownPanel({ data }: { data: SpanChildrenResponse }) {
  // Residual = parent wall-time not covered by any direct subspan. Often
  // the largest single piece on a coarsely-instrumented parent, so we
  // surface it explicitly rather than hiding it.
  const childSum = data.children.reduce((a, c) => a + c.sum_ms, 0);
  const residualMs = Math.max(0, data.parent_sum_ms - childSum);
  const totalForBar = data.parent_sum_ms || 1;
  const segments: ReadonlyArray<{
    name: string;
    sum_ms: number;
    color: string;
    count?: number;
    mean_ms?: number;
  }> = [
    ...data.children.map((c) => ({
      name: c.name,
      sum_ms: c.sum_ms,
      count: c.count,
      mean_ms: c.mean_ms,
      color: nameToHsl(c.name),
    })),
    ...(residualMs > 0
      ? [
          {
            name: "self / unaccounted",
            sum_ms: residualMs,
            color: "color-mix(in oklab, var(--fg-muted) 50%, transparent)",
          },
        ]
      : []),
  ];

  return (
    <div className="flex flex-col gap-2" style={{ marginTop: "var(--space-2)" }}>
      <div className="text-fg-muted text-xs">
        child breakdown · {data.parent_count}{" "}
        {data.parent_count === 1 ? "parent" : "parents"} totalling{" "}
        {fmtMs(data.parent_sum_ms)}
      </div>
      <div
        style={{
          display: "flex",
          height: 14,
          width: "100%",
          borderRadius: "var(--radius-1)",
          overflow: "hidden",
          border: "1px solid var(--border-subtle)",
        }}
      >
        {segments.map((s) => (
          <div
            key={s.name}
            title={`${s.name}: ${fmtMs(s.sum_ms)} (${pct(s.sum_ms, totalForBar)})`}
            style={{
              width: `${(s.sum_ms / totalForBar) * 100}%`,
              background: s.color,
            }}
          />
        ))}
      </div>
      <table
        className="tracks tabular"
        style={{ fontFamily: "var(--font-mono)", fontSize: 12 }}
      >
        <thead>
          <tr>
            <th style={{ width: 14 }} />
            <th>child</th>
            <th style={{ textAlign: "right" }}>count</th>
            <th style={{ textAlign: "right" }}>sum</th>
            <th style={{ textAlign: "right" }}>mean</th>
            <th style={{ textAlign: "right" }}>share</th>
          </tr>
        </thead>
        <tbody>
          {segments.map((s) => (
            <tr key={s.name}>
              <td>
                <span
                  style={{
                    display: "inline-block",
                    width: 10,
                    height: 10,
                    borderRadius: 2,
                    background: s.color,
                  }}
                />
              </td>
              <td className="col-title">{s.name}</td>
              <td className="col-time">{s.count ?? "—"}</td>
              <td className="col-time">{fmtMs(s.sum_ms)}</td>
              <td className="col-time">
                {s.mean_ms !== undefined ? fmtMs(s.mean_ms) : "—"}
              </td>
              <td className="col-time">{pct(s.sum_ms, totalForBar)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function pct(part: number, total: number): string {
  if (total <= 0) return "0%";
  const p = (part / total) * 100;
  if (p >= 10) return `${p.toFixed(0)}%`;
  if (p >= 1) return `${p.toFixed(1)}%`;
  return `${p.toFixed(2)}%`;
}

// --- trace list (level 3: individual invocations) ----------------------

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

// --- scatter plot ------------------------------------------------------

function SpanScatter({
  points,
  p50,
  p95,
  p99,
  max,
}: {
  points: SpanSeriesPoint[];
  p50: number;
  p95: number;
  p99: number;
  max: number;
}) {
  // Empty windows are common when the chosen range predates the ring's
  // oldest sample — show a friendly note rather than a blank SVG.
  if (points.length === 0) {
    return (
      <p className="text-fg-faint text-sm" style={{ marginTop: 4 }}>
        no samples in the selected range.
      </p>
    );
  }
  // The store returns newest-first; flip for time-axis plotting.
  const sorted = [...points].sort((a, b) => a.end_ms - b.end_ms);
  const xMin = sorted[0]!.end_ms;
  const xMax = sorted[sorted.length - 1]!.end_ms;
  // Pad y-axis above max so the topmost point doesn't kiss the border.
  // Use the row's overall p99 as a fallback when this window is empty
  // of outliers — keeps zoom consistent across ranges.
  const yMaxData = Math.max(...sorted.map((p) => p.duration_ms), 1);
  const yMax = Math.max(yMaxData, p99) * 1.08;
  const W = 800;
  const H = 220;
  const PAD_L = 56;
  const PAD_R = 12;
  const PAD_T = 8;
  const PAD_B = 26;
  const plotW = W - PAD_L - PAD_R;
  const plotH = H - PAD_T - PAD_B;

  const xRange = Math.max(xMax - xMin, 1);
  const x = (t: number) => PAD_L + ((t - xMin) / xRange) * plotW;
  const y = (v: number) => PAD_T + plotH - (v / yMax) * plotH;

  // Subsample for >2k points: SVG circle elements get expensive past
  // a few thousand. Pick evenly-spaced samples; the dominant signal
  // (trend, outliers) survives.
  const MAX_DOTS = 2_000;
  const stride = Math.max(1, Math.ceil(sorted.length / MAX_DOTS));
  const visible = sorted.filter((_, i) => i % stride === 0);

  const yTickValues = niceTicks(yMax, 4);
  const xTicks = xTickLabels(xMin, xMax, 5);

  return (
    <div style={{ width: "100%", overflow: "hidden" }}>
      <svg
        viewBox={`0 0 ${W} ${H}`}
        width="100%"
        style={{
          display: "block",
          background: "color-mix(in oklab, var(--surface-0) 60%, transparent)",
          borderRadius: "var(--radius-2)",
          border: "1px solid var(--border-subtle)",
        }}
        role="img"
        aria-label={`scatter plot of ${visible.length} samples`}
      >
        {/* y-axis grid + tick labels */}
        {yTickValues.map((v) => (
          <g key={`y-${v}`}>
            <line
              x1={PAD_L}
              x2={W - PAD_R}
              y1={y(v)}
              y2={y(v)}
              stroke="var(--border-subtle)"
              strokeDasharray="2 3"
              opacity={0.5}
            />
            <text
              x={PAD_L - 6}
              y={y(v) + 3}
              fontSize={10}
              textAnchor="end"
              fill="var(--fg-muted)"
              fontFamily="var(--font-mono)"
            >
              {fmtMs(v)}
            </text>
          </g>
        ))}

        {/* p50 / p95 / p99 reference lines (bucket-wide, not window) */}
        <ReferenceLine y={y(p50)} x1={PAD_L} x2={W - PAD_R} color="#22c55e" label="p50" />
        <ReferenceLine y={y(p95)} x1={PAD_L} x2={W - PAD_R} color="#eab308" label="p95" />
        <ReferenceLine y={y(p99)} x1={PAD_L} x2={W - PAD_R} color="#ef4444" label="p99" />

        {/* x-axis ticks */}
        {xTicks.map((t) => (
          <g key={`x-${t.ms}`}>
            <line
              x1={x(t.ms)}
              x2={x(t.ms)}
              y1={H - PAD_B}
              y2={H - PAD_B + 3}
              stroke="var(--border-subtle)"
            />
            <text
              x={x(t.ms)}
              y={H - PAD_B + 15}
              fontSize={10}
              textAnchor="middle"
              fill="var(--fg-muted)"
              fontFamily="var(--font-mono)"
            >
              {t.label}
            </text>
          </g>
        ))}

        {/* data points */}
        {visible.map((p, i) => (
          <circle
            key={i}
            cx={x(p.end_ms)}
            cy={y(p.duration_ms)}
            r={1.8}
            fill="var(--accent)"
            opacity={0.7}
          >
            <title>
              {new Date(p.end_ms).toISOString()} — {fmtMs(p.duration_ms)}
            </title>
          </circle>
        ))}

        {/* axis frame */}
        <line
          x1={PAD_L}
          y1={H - PAD_B}
          x2={W - PAD_R}
          y2={H - PAD_B}
          stroke="var(--border-subtle)"
        />
        <line x1={PAD_L} y1={PAD_T} x2={PAD_L} y2={H - PAD_B} stroke="var(--border-subtle)" />
      </svg>
      <div className="text-fg-faint text-xs" style={{ marginTop: 4 }}>
        max in row: {fmtMs(max)} · showing {visible.length} of {sorted.length}{" "}
        {sorted.length === 1 ? "sample" : "samples"}
        {stride > 1 ? ` (every ${stride}th)` : ""}
      </div>
    </div>
  );
}

function ReferenceLine({
  y,
  x1,
  x2,
  color,
  label,
}: {
  y: number;
  x1: number;
  x2: number;
  color: string;
  label: string;
}) {
  return (
    <g>
      <line x1={x1} x2={x2} y1={y} y2={y} stroke={color} strokeDasharray="4 2" opacity={0.7} />
      <text
        x={x2 - 4}
        y={y - 3}
        fontSize={9}
        textAnchor="end"
        fill={color}
        fontFamily="var(--font-mono)"
      >
        {label}
      </text>
    </g>
  );
}

// Pick ~n round-number tick values between 0 and `max`. We use a 1/2/5
// step scheme rather than d3-style: good enough and keeps the bundle
// free of a charting dep.
function niceTicks(max: number, target: number): number[] {
  if (max <= 0) return [0];
  const rough = max / target;
  const pow = 10 ** Math.floor(Math.log10(rough));
  const norm = rough / pow;
  let step = pow;
  if (norm >= 5) step = 5 * pow;
  else if (norm >= 2) step = 2 * pow;
  const out: number[] = [];
  for (let v = 0; v <= max; v += step) out.push(v);
  return out;
}

interface XTick {
  ms: number;
  label: string;
}

function xTickLabels(xMin: number, xMax: number, count: number): XTick[] {
  if (xMax <= xMin) return [{ ms: xMin, label: shortTime(xMin) }];
  const out: XTick[] = [];
  for (let i = 0; i < count; i++) {
    const ms = xMin + ((xMax - xMin) * i) / (count - 1);
    out.push({ ms, label: shortTime(ms) });
  }
  return out;
}

function shortTime(ms: number): string {
  const d = new Date(ms);
  const today = new Date();
  const sameDay =
    d.getFullYear() === today.getFullYear() &&
    d.getMonth() === today.getMonth() &&
    d.getDate() === today.getDate();
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  if (sameDay) return `${hh}:${mm}`;
  const mon = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${mon}-${day} ${hh}:${mm}`;
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
