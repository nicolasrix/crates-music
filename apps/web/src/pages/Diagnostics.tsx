import { useQuery } from "@tanstack/react-query";
import { useMemo, useState } from "react";

import {
  HistogramBucket,
  TraceEntry,
  fetchHistogram,
  fetchQueueDepth,
  fetchTraces,
} from "../api/diagnostics";
import { Layout } from "../components/Layout";

// 5 s refresh keeps the page "live enough" for a human watching ingest
// progress without hammering the gateway. Slow enough that the SQLite
// reads remain noise-level (~ms per query at 100 k-row ring cap).
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
      // exactOptionalPropertyTypes: a missing key and `undefined` are
      // not interchangeable. Build the arg object without `name` when
      // the filter is empty.
      fetchTraces(nameFilter ? { limit: TRACES_LIMIT, name: nameFilter } : { limit: TRACES_LIMIT }),
    refetchInterval: REFRESH_MS,
  });

  return (
    <Layout>
      <h1 className="text-2xl font-semibold mb-6">Diagnostics</h1>

      <Section title="Ingest queue">
        {queue.error && <ErrorLine error={queue.error} />}
        {queue.data && <QueueDepth data={queue.data} />}
      </Section>

      <Section title="Span duration histogram">
        {histogram.error && <ErrorLine error={histogram.error} />}
        {histogram.data && <HistogramTable buckets={histogram.data.buckets} />}
      </Section>

      <Section title="Recent traces">
        <div className="mb-3 flex items-center gap-2">
          <label className="text-sm text-stone-400">filter by name:</label>
          <select
            className="bg-stone-900 border border-stone-800 rounded px-2 py-1 text-sm"
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
          <span className="text-xs text-stone-500">
            limit {TRACES_LIMIT}, refresh {REFRESH_MS / 1000}s
          </span>
        </div>
        {traces.error && <ErrorLine error={traces.error} />}
        {traces.data && <TraceList entries={traces.data.traces} />}
      </Section>
    </Layout>
  );
}

// --- layout helpers --------------------------------------------------------

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="mb-10">
      <h2 className="text-lg font-medium mb-3 text-stone-200">{title}</h2>
      {children}
    </section>
  );
}

function ErrorLine({ error }: { error: unknown }) {
  return <p className="text-red-400 text-sm">error: {(error as Error).message}</p>;
}

// --- queue depth -----------------------------------------------------------

function QueueDepth({
  data,
}: {
  data: { model_version: string; not_started: number; in_progress: number; done: number; failed: number };
}) {
  const tiles = [
    { label: "not started", value: data.not_started, tone: "text-stone-200" },
    { label: "in progress", value: data.in_progress, tone: "text-amber-300" },
    { label: "done", value: data.done, tone: "text-emerald-300" },
    { label: "failed", value: data.failed, tone: data.failed > 0 ? "text-red-400" : "text-stone-500" },
  ];
  return (
    <div>
      <p className="text-xs text-stone-500 mb-2">model: {data.model_version}</p>
      <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
        {tiles.map((t) => (
          <div
            key={t.label}
            className="border border-stone-800 rounded p-3 bg-stone-900/40"
          >
            <p className="text-xs text-stone-500 uppercase tracking-wide">
              {t.label}
            </p>
            <p className={`text-2xl font-semibold ${t.tone}`}>{t.value}</p>
          </div>
        ))}
      </div>
    </div>
  );
}

// --- histogram -------------------------------------------------------------

function HistogramTable({ buckets }: { buckets: HistogramBucket[] }) {
  if (buckets.length === 0) {
    return <p className="text-sm text-stone-500">no spans recorded yet</p>;
  }
  // Scale all bars against the widest span across all buckets — makes
  // an "embedder.embed_audio" at p99=8000ms visibly tower over a
  // "trace_store.insert_batch" at p99=1ms. Per-row scaling would hide
  // that.
  const maxMs = Math.max(...buckets.map((b) => b.max_ms), 1);
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-sm">
        <thead className="text-xs text-stone-500 uppercase tracking-wide">
          <tr>
            <th className="text-left pb-2">name</th>
            <th className="text-right pb-2 px-3">count</th>
            <th className="text-right pb-2 px-3">min</th>
            <th className="text-right pb-2 px-3">p50</th>
            <th className="text-right pb-2 px-3">p95</th>
            <th className="text-right pb-2 px-3">p99</th>
            <th className="text-right pb-2 px-3">max</th>
            <th className="text-left pb-2 pl-4 w-1/3">distribution (p50/p95/p99 / max)</th>
          </tr>
        </thead>
        <tbody>
          {buckets.map((b) => (
            <tr key={b.name} className="border-t border-stone-800">
              <td className="py-2 font-mono text-stone-200">{b.name}</td>
              <td className="py-2 px-3 text-right tabular-nums">{b.count}</td>
              <td className="py-2 px-3 text-right tabular-nums text-stone-400">
                {fmtMs(b.min_ms)}
              </td>
              <td className="py-2 px-3 text-right tabular-nums">{fmtMs(b.p50_ms)}</td>
              <td className="py-2 px-3 text-right tabular-nums">{fmtMs(b.p95_ms)}</td>
              <td className="py-2 px-3 text-right tabular-nums">{fmtMs(b.p99_ms)}</td>
              <td className="py-2 px-3 text-right tabular-nums text-stone-400">
                {fmtMs(b.max_ms)}
              </td>
              <td className="py-2 pl-4">
                <BoxPlot
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

function BoxPlot({
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
    <div className="relative h-4 bg-stone-900 rounded overflow-hidden">
      <div className="absolute inset-y-0 left-0 bg-emerald-700/60" style={{ width: pct(p50) }} />
      <div className="absolute inset-y-0 left-0 bg-amber-600/40" style={{ width: pct(p95) }} />
      <div className="absolute inset-y-0 left-0 bg-red-600/30" style={{ width: pct(p99) }} />
      <div
        className="absolute top-0 bottom-0 w-px bg-stone-300"
        style={{ left: pct(max) }}
        title={`max ${fmtMs(max)}`}
      />
    </div>
  );
}

// --- trace list / waterfall ------------------------------------------------

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
  // Newest trace first (by latest end_ms).
  return [...map.values()].sort((a, b) => b.end_ms - a.end_ms);
}

function TraceList({ entries }: { entries: TraceEntry[] }) {
  const groups = useMemo(() => groupByTrace(entries), [entries]);
  if (groups.length === 0) {
    return <p className="text-sm text-stone-500">no traces in window</p>;
  }
  return (
    <div className="space-y-3">
      {groups.map((g) => (
        <TraceCard key={g.trace_id} group={g} />
      ))}
    </div>
  );
}

function TraceCard({ group }: { group: Group }) {
  const [open, setOpen] = useState(false);
  const totalMs = Math.max(group.end_ms - group.start_ms, 1);
  // Sort spans by start so the waterfall reads top-down chronologically.
  const sorted = [...group.spans].sort((a, b) => a.start_ms - b.start_ms);
  return (
    <div className="border border-stone-800 rounded bg-stone-900/40">
      <button
        className="w-full flex items-center justify-between px-3 py-2 text-left"
        onClick={() => setOpen((v) => !v)}
      >
        <span className="font-mono text-xs text-stone-400">{group.trace_id}</span>
        <span className="text-xs text-stone-500">
          {sorted.length} span{sorted.length === 1 ? "" : "s"} · {fmtMs(totalMs)}
        </span>
      </button>
      <div className="px-3 pb-3 space-y-1">
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
  const widthPct = Math.max((span.duration_ms / totalMs) * 100, 0.3); // floor so 0-ms spans stay visible
  const color = nameToHsl(span.name);
  return (
    <div>
      <div className="flex items-center gap-2 text-xs">
        <span className="font-mono text-stone-300 w-56 truncate" title={span.name}>
          {span.name}
        </span>
        <div className="flex-1 relative h-4 bg-stone-950/60 rounded overflow-hidden">
          <div
            className="absolute inset-y-0 rounded"
            style={{
              left: `${offsetPct}%`,
              width: `${widthPct}%`,
              backgroundColor: color,
            }}
            title={`${span.name}: ${fmtMs(span.duration_ms)}`}
          />
        </div>
        <span className="tabular-nums text-stone-400 w-16 text-right">
          {fmtMs(span.duration_ms)}
        </span>
      </div>
      {expanded && Object.keys(span.fields).length > 0 && (
        <pre className="mt-1 ml-56 text-[11px] text-stone-500 bg-stone-950/60 px-2 py-1 rounded overflow-x-auto">
          {JSON.stringify(span.fields, null, 0)}
        </pre>
      )}
    </div>
  );
}

// --- helpers ---------------------------------------------------------------

function fmtMs(n: number): string {
  if (n < 1) return "<1ms";
  if (n < 1000) return `${Math.round(n)}ms`;
  return `${(n / 1000).toFixed(2)}s`;
}

// Stable color per span name. djb2 hash → HSL with fixed S/L so every
// bar has comparable saturation regardless of name length.
function nameToHsl(name: string): string {
  let h = 5381;
  for (let i = 0; i < name.length; i++) {
    h = ((h << 5) + h + name.charCodeAt(i)) | 0;
  }
  const hue = ((h % 360) + 360) % 360;
  return `hsl(${hue}, 60%, 55%)`;
}
