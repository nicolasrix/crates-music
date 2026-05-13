// /diagnostics/tracing — span duration histogram + recent traces.
//
// These two panels share data: the trace-filter dropdown is populated
// from the histogram response. Splitting them would force the traces
// page to refetch the histogram just for the dropdown — so they live
// together on one page.

import { useQuery } from "@tanstack/react-query";
import { useMemo, useState } from "react";

import {
  HistogramBucket,
  TraceEntry,
  fetchHistogram,
  fetchTraces,
} from "../../api/diagnostics";
import { Layout } from "../../components/Layout";
import { fmtMs } from "../../utils/format";
import { DiagSection, ErrorLine, REFRESH_MS } from "./shared";

const TRACES_LIMIT = 200;

export function Tracing() {
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
    <Layout breadcrumb="diagnostics / tracing">
      <div className="section">
        <div className="section-head">
          <h2>tracing</h2>
          <span className="count">refresh {REFRESH_MS / 1000}s</span>
        </div>

        <DiagSection title="span duration histogram">
          {histogram.error && <ErrorLine error={histogram.error} />}
          {histogram.data && <HistogramTable buckets={histogram.data.buckets} />}
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

// Stable color per span name for the waterfall. djb2 hash → HSL.
function nameToHsl(name: string): string {
  let h = 5381;
  for (let i = 0; i < name.length; i++) {
    h = ((h << 5) + h + name.charCodeAt(i)) | 0;
  }
  const hue = ((h % 360) + 360) % 360;
  return `hsl(${hue}, 60%, 55%)`;
}
