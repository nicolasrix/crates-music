// Latent-space scatter for the diagnostics surface. Renders all
// embedded tracks as 2-D UMAP coords on a canvas, with hover-tooltip
// and click-to-play. Mounted at /diagnostics/latent.
//
// Two design choices worth flagging:
//
//   1. We render to a single <canvas> rather than 10k <circle> SVG
//      nodes. At our scale (~10⁴ points) a Path2D dot pass is one frame
//      of CPU; the equivalent SVG would push the layout engine past
//      interactive budgets on hover.
//
//   2. Hover is brute-force-O(N) — we scan every point per mousemove,
//      not a spatial index. At 10k points that's well under a frame at
//      60Hz; introducing a kd-tree would buy nothing measurable and
//      cost obvious complexity. If we ever land 100k+ points, this is
//      the first thing to optimise.
//
// Coords come from the gateway-side ProjectionStore, populated by the
// Python reducer (services/embedder/embedder/reduce.py). The page does
// no projection work itself.

import { useQuery } from "@tanstack/react-query";
import React, { Suspense, useEffect, useMemo, useRef, useState } from "react";

import { getSong } from "../api/client";
import {
  fetchRecommendLatentNeighbours,
  fetchRecommendLatentSpace,
  fetchRecommendSessions,
  LatentNeighbourEntry,
  LatentSpacePoint,
  SessionItem,
} from "../api/diagnostics";
import { Layout } from "../components/Layout";
import { Link } from "../router";
import { usePlayback } from "../sync/usePlayback";

// 3-D scene + three.js are heavy (~250 KB gzipped). The rest of the app
// shouldn't pay that cost; lazy() defers the load to the first render
// of UMAP-z mode. Suspense boundary below absorbs the loading flash.
const LatentSpace3D = React.lazy(() =>
  import("./LatentSpace3D").then((m) => ({ default: m.LatentSpace3D })),
);
import {
  bucketByGenre,
  colorChannelValue,
  computeBounds,
  cosineDistanceToWidth,
  normalizeInto01,
  pickNearestPoint,
  pickPointsByIds,
  rangeOfFiniteValues,
  scaleToCanvas,
  sessionHue,
  viridis,
  type ContinuousColorMode,
  type DataBounds,
  type CanvasGeometry,
  type GenreBucket,
  type ScatterPoint,
} from "./latentSpace";

/// Color-by selector states. "genre" keeps the legacy bucketed palette;
/// the rest are continuous channels driven through the viridis
/// gradient — see `ContinuousColorMode` in `./latentSpace`.
type ColorMode = "genre" | ContinuousColorMode;

const COLOR_MODES: ReadonlyArray<{ value: ColorMode; label: string }> = [
  { value: "genre", label: "genre" },
  { value: "pc1", label: "PC1" },
  { value: "pc2", label: "PC2" },
  { value: "pc3", label: "PC3" },
  { value: "pc4", label: "PC4" },
  { value: "umap_z", label: "UMAP z" },
];

function continuousValue(
  point: LatentSpacePoint,
  mode: ColorMode,
): number | null {
  return mode === "genre" ? null : colorChannelValue(point, mode);
}

const CANVAS_PADDING = 24;
const POINT_RADIUS = 2.5;
// Slightly larger than POINT_RADIUS so the hover target is forgiving
// — picking a 2.5px dot exactly is annoying on a touchpad.
const HOVER_RADIUS = 8;
// Cap on named-genre buckets in the legend. Subsonic libraries
// routinely have 50+ rare tags; we keep the top 10 (matches the
// palette size) and roll the rest into 'Other'.
const TOP_GENRE_BUCKETS = 10;
// Lower bound so the chart stays usable on narrow viewports; without
// this, a phone-width window would collapse the scatter to a strip.
const MIN_CANVAS_WIDTH = 320;
// Cap height to a fraction of the viewport so the chart never pushes
// the legend / topbar off-screen on tall windows.
const MAX_HEIGHT_VH = 0.78;
// How many recent sessions to make pickable. 50 is the gateway default;
// keep it explicit so changes to the default don't silently grow the
// dropdown.
const SESSION_FETCH_LIMIT = 50;
/// Sentinel for "render every session at once".
const SESSION_ALL = "__all__";
/// Sentinel for "render no path overlay".
const SESSION_NONE = "__none__";

// Stroke-width range for session paths. The recommender's own metric
// is cosine distance, in `[0, 2]`; in practice CLAP cosines cluster in
// `[0, 1]`, so cap there. Closer in latent space → thicker stroke.
const PATH_WIDTH_RANGE = { minWidth: 0.6, maxWidth: 4, cap: 1 } as const;

export function LatentSpace() {
  // Dropdown state for the session overlay. Defaults to "none" — the
  // scatter is useful without a path, and fetching event-bundled
  // sessions has a real cost on the gateway side.
  const [sessionPick, setSessionPick] = useState<string>(SESSION_NONE);
  // What channel each dot's colour encodes. Genre / PC modes anchor on
  // the 2D-UMAP layout; "umap_z" mode anchors on the 3D-UMAP layout
  // (so x, y, z all come from the same 3-component run). The active
  // dataset below pivots on this.
  const [colorMode, setColorMode] = useState<ColorMode>("genre");

  // Two parallel queries — one per layout. Both fetched eagerly so the
  // UMAP-z toggle is instantaneous after the initial load. TanStack
  // Query caches each independently by its queryKey.
  const data2dQ = useQuery({
    queryKey: ["diag", "latent_space", "prefer:2d"],
    queryFn: () => fetchRecommendLatentSpace({ prefer: "2d" }),
    refetchInterval: 30_000,
  });
  const data3dQ = useQuery({
    queryKey: ["diag", "latent_space", "prefer:3d"],
    queryFn: () => fetchRecommendLatentSpace({ prefer: "3d" }),
    refetchInterval: 30_000,
  });

  // Active dataset: 3D run drives the canvas only when the user picks
  // the "UMAP z" colour mode. Every other colour mode renders against
  // the 2D-UMAP layout so PC/genre intuitions stay anchored.
  const data = colorMode === "umap_z" ? data3dQ.data : data2dQ.data;
  const isLoading = colorMode === "umap_z" ? data3dQ.isLoading : data2dQ.isLoading;
  const error = colorMode === "umap_z" ? data3dQ.error : data2dQ.error;

  // The colour-mode picker needs to know whether UMAP-z is available
  // *across both queries* — disabling the option until the 3D run's
  // data lands. We feed it the 3D points so its `points.some(p.z!==null)`
  // check sees the right dataset.
  const colorModePoints = data2dQ.data?.points ?? [];
  const hasUmapZ = (data3dQ.data?.points ?? []).some((p) => p.z !== null);

  // Only fetch sessions+events once the user actually picks one (or
  // "all"). Listing sessions without events is cheap; including events
  // is per-segment embedding lookups, which we don't want to pay on
  // every page load.
  const wantSessions = sessionPick !== SESSION_NONE;
  const sessionsQ = useQuery({
    queryKey: ["diag", "sessions_with_events", SESSION_FETCH_LIMIT],
    queryFn: () =>
      fetchRecommendSessions({
        limit: SESSION_FETCH_LIMIT,
        includeEvents: true,
      }),
    enabled: wantSessions,
    refetchInterval: 30_000,
  });

  return (
    <Layout breadcrumb="diagnostics / latent space">
      <div className="section">
        <div className="section-head">
          <h2>latent space</h2>
          <span className="count">
            <Link to="/diagnostics">← back to diagnostics</Link>
          </span>
        </div>

        {error && (
          <p className="text-danger text-sm">
            error: {(error as Error).message}
          </p>
        )}

        {data && (
          <>
            <ModelHint modelVersion={data.model_version} />
            <SessionPicker
              value={sessionPick}
              sessions={sessionsQ.data?.items ?? []}
              loading={wantSessions && sessionsQ.isFetching}
              onChange={setSessionPick}
            />
            <ColorModePicker
              value={colorMode}
              points={colorModePoints}
              umapZAvailable={hasUmapZ}
              onChange={setColorMode}
            />
            {data.points.length === 0 ? (
              <EmptyHint
                hasProjection={data.proj_version !== null}
                modelVersion={data.model_version}
                isUmapZ={colorMode === "umap_z"}
              />
            ) : colorMode === "umap_z" ? (
              // The 3-D run drives the camera, layout, and colour. The
              // Suspense fallback shows briefly the first time the user
              // picks UMAP-z (three.js + the scene module load on demand).
              <Suspense fallback={<p className="text-sm">loading 3-D view…</p>}>
                <LatentSpace3D
                  points={data.points}
                  sessions={selectSessionsForOverlay(
                    sessionPick,
                    sessionsQ.data?.items ?? []
                  )}
                />
              </Suspense>
            ) : (
              <ScatterCanvas
                points={data.points}
                colorMode={colorMode}
                sessions={selectSessionsForOverlay(
                  sessionPick,
                  sessionsQ.data?.items ?? []
                )}
              />
            )}
          </>
        )}

        {isLoading && !data && <p className="text-sm">loading projection…</p>}
      </div>
    </Layout>
  );
}

/// Decide which sessions to draw for the current dropdown selection.
/// Kept pure (no hooks) so the picker logic stays separable from
/// rendering.
function selectSessionsForOverlay(
  pick: string,
  sessions: readonly SessionItem[]
): SessionItem[] {
  if (pick === SESSION_NONE) return [];
  if (pick === SESSION_ALL) return [...sessions];
  return sessions.filter((s) => s.session_id === pick);
}

function ColorModePicker({
  value,
  points,
  umapZAvailable,
  onChange,
}: {
  value: ColorMode;
  points: readonly LatentSpacePoint[];
  /** Whether the 3-D UMAP companion has loaded with z values
   *  populated. Decoupled from `points` because the picker is fed the
   *  2-D points (which never carry z), while UMAP-z availability is
   *  determined by a separate query against the 3-D run. */
  umapZAvailable: boolean;
  onChange: (v: ColorMode) => void;
}) {
  // A PC mode without any rows that have that PC is misleading — the
  // dropdown would silently render every dot grey. Disable each PC
  // entry the projection didn't populate so the user knows whether
  // to re-run the reducer.
  const pcHasData: Record<Exclude<ColorMode, "genre">, boolean> = {
    pc1: points.some((p) => p.pc1 !== null),
    pc2: points.some((p) => p.pc2 !== null),
    pc3: points.some((p) => p.pc3 !== null),
    pc4: points.some((p) => p.pc4 !== null),
    umap_z: umapZAvailable,
  };
  return (
    <div
      style={{
        display: "flex",
        gap: "var(--space-3)",
        alignItems: "center",
        marginBottom: "var(--space-3)",
        flexWrap: "wrap",
      }}
    >
      <label className="text-sm" style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
        colour by
        <InfoTooltip>
          <strong>Different reductions of the same 512-D CLAP space.</strong>
          <ul style={{ margin: "6px 0 0 0", paddingLeft: 18 }}>
            <li>
              <strong>x, y</strong> — UMAP (non-linear, locally faithful).
              Tight clusters mean "these sound similar."
            </li>
            <li>
              <strong>colour → PCn</strong> — the n-th PCA component on
              the original embedding. Linear, ordered by variance,
              orthogonal to PC1…PC(n-1). Rendered on top of the 2-D
              UMAP layout.
            </li>
            <li>
              <strong>colour → UMAP z</strong> — switches the canvas to
              a real 3-D point cloud (independent UMAP run with
              proj_version suffix <code>-d3</code>). Drag to rotate,
              scroll to zoom; colour is the third axis. Only enabled
              when a 3-D run exists.
            </li>
          </ul>
          <div style={{ marginTop: 6 }}>
            Picking the colour mode also picks the layout: PC / genre
            modes use the 2-D reduction, "UMAP z" uses the 3-D one.
            They're independent fits, so dot positions <em>will</em>{" "}
            move when you toggle between them.
          </div>
          <div style={{ marginTop: 6 }}>
            The colour axis is <em>not</em> geometrically aligned with
            x/y — moving up the gradient does not correspond to a
            direction on the canvas. PC1 carries the most variance;
            higher PCs reveal independent extra axes the UMAP layout
            can't preserve linearly.
          </div>
        </InfoTooltip>
        &nbsp;
        <select
          value={value}
          onChange={(e) => onChange(e.target.value as ColorMode)}
          className="search-input"
          style={{ fontFamily: "var(--font-mono)", width: "auto", paddingLeft: 12 }}
        >
          {COLOR_MODES.map((m) => {
            const disabled = m.value !== "genre" && !pcHasData[m.value];
            return (
              <option key={m.value} value={m.value} disabled={disabled}>
                {m.label}
                {disabled ? " — not in this projection" : ""}
              </option>
            );
          })}
        </select>
      </label>
      {value !== "genre" && (
        <span className="text-sm" style={{ color: "var(--muted)" }}>
          viridis · low → high · grey = no value
        </span>
      )}
    </div>
  );
}

function SessionPicker({
  value,
  sessions,
  loading,
  onChange,
}: {
  value: string;
  sessions: readonly SessionItem[];
  loading: boolean;
  onChange: (v: string) => void;
}) {
  const fmtTs = (ms: number) =>
    new Date(ms).toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  return (
    <div
      style={{
        display: "flex",
        gap: "var(--space-3)",
        alignItems: "center",
        marginBottom: "var(--space-3)",
        flexWrap: "wrap",
      }}
    >
      <label className="text-sm">
        session&nbsp;
        <select
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className="search-input"
          style={{ fontFamily: "var(--font-mono)", width: "auto", paddingLeft: 12 }}
        >
          <option value={SESSION_NONE}>— none —</option>
          <option value={SESSION_ALL}>show all ({sessions.length})</option>
          {sessions.map((s) => (
            <option key={s.session_id} value={s.session_id}>
              {fmtTs(s.started_ms)} · {s.items_count} items · {s.event_count}{" "}
              evts
            </option>
          ))}
        </select>
      </label>
      {loading && (
        <span className="text-sm" style={{ color: "var(--muted)" }}>
          loading sessions…
        </span>
      )}
      {value !== SESSION_NONE && (
        <span className="text-sm" style={{ color: "var(--muted)" }}>
          line thickness ∝ closeness in CLAP space · dashed = track not in this
          projection
        </span>
      )}
    </div>
  );
}

/// Read-only badge replacing the old proj_version dropdown. The active
/// projection now follows the colour-mode selector (2-D for most modes,
/// 3-D when "UMAP z" is picked) — exposing a manual proj_version
/// dropdown alongside that logic invited the bug the user just hit
/// ("why are the dots in the same place?"). We still echo the embedding
/// model since it's useful context for the page.
function ModelHint({ modelVersion }: { modelVersion: string }) {
  return (
    <div
      style={{
        display: "flex",
        gap: "var(--space-3)",
        alignItems: "center",
        marginBottom: "var(--space-3)",
        flexWrap: "wrap",
      }}
    >
      <span
        className="text-sm"
        style={{ color: "var(--muted)", fontFamily: "var(--font-mono)" }}
      >
        model: {modelVersion}
      </span>
    </div>
  );
}

function EmptyHint({
  hasProjection,
  modelVersion,
  isUmapZ,
}: {
  hasProjection: boolean;
  modelVersion: string;
  /** True when the user is on the UMAP-z colour mode — points being
   *  empty means the 3-D reducer hasn't run, not the 2-D one. */
  isUmapZ: boolean;
}) {
  if (!hasProjection) {
    const components = isUmapZ ? " --n-components 3" : "";
    return (
      <p className="text-sm" style={{ color: "var(--muted)" }}>
        no {isUmapZ ? "3-D" : "2-D"} projection has been written yet for{" "}
        <code>{modelVersion}</code>. Run{" "}
        <code>
          uv run --extra reduce python -m embedder.reduce --db &lt;path&gt; --model-version{" "}
          {modelVersion}
          {components}
        </code>{" "}
        in <code>services/embedder/</code> to populate it.
      </p>
    );
  }
  return (
    <p className="text-sm" style={{ color: "var(--muted)" }}>
      this projection is empty.
    </p>
  );
}

function ScatterCanvas({
  points,
  sessions,
  colorMode,
}: {
  points: LatentSpacePoint[];
  sessions: readonly SessionItem[];
  colorMode: ColorMode;
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const [hover, setHover] = useState<LatentSpacePoint | null>(null);
  // Debounced version of `hover?.track_id` — dragging across the
  // canvas would otherwise spam the gateway with one neighbour-fetch
  // per pointer-move event. 150 ms is a comfortable "settled hover"
  // window; TanStack Query then caches per-track, so repeat hovers
  // are free.
  const [debouncedHoverId, setDebouncedHoverId] = useState<string | null>(null);
  useEffect(() => {
    const id = hover?.track_id ?? null;
    if (id === debouncedHoverId) return;
    const handle = window.setTimeout(() => setDebouncedHoverId(id), 150);
    return () => window.clearTimeout(handle);
  }, [hover, debouncedHoverId]);
  const neighboursQ = useQuery({
    queryKey: ["latent-neighbours", debouncedHoverId],
    queryFn: () =>
      fetchRecommendLatentNeighbours({ trackId: debouncedHoverId!, k: 10 }),
    enabled: !!debouncedHoverId,
    // Embeddings don't change for an existing track, so the answer is
    // cacheable for the whole page lifetime. Keep gcTime modest so
    // the cache doesn't grow unbounded if the user hovers many
    // points; 5 min is the TanStack default.
    staleTime: 60 * 60 * 1000,
  });
  const playback = usePlayback();
  // Resolving track_id → Track via Subsonic getSong is a separate
  // network round-trip; we surface a transient "loading" state so a
  // mis-click can be acknowledged.
  const [playingTrackId, setPlayingTrackId] = useState<string | null>(null);
  // Legend filter — click a bucket to toggle its visibility. Stored as
  // a Set so the canvas + picker can both consult it cheaply.
  const [hiddenBuckets, setHiddenBuckets] = useState<Set<string>>(new Set());
  // Live canvas size, driven by a ResizeObserver on the wrapper. Width
  // tracks the available column; height matches the data's natural
  // aspect ratio (computed from bounds, with fallback 4:3 before the
  // first measurement). Capped at MAX_HEIGHT_VH so the chart never
  // pushes the legend below the fold on tall viewports.
  const [size, setSize] = useState({ width: MIN_CANVAS_WIDTH, height: Math.round(MIN_CANVAS_WIDTH * 0.75) });

  const bounds: DataBounds | null = useMemo(() => computeBounds(points), [points]);

  // Recompute size whenever the wrapper changes width or the data
  // aspect ratio changes. ResizeObserver fires on layout shifts (window
  // resize, sidebar toggle, font reflow) — no manual `resize` listener
  // needed. The wrapper's width is the source of truth; we derive a
  // height to preserve the data's aspect ratio.
  useEffect(() => {
    const wrap = wrapRef.current;
    if (!wrap) return;
    const aspect =
      bounds && bounds.maxX > bounds.minX && bounds.maxY > bounds.minY
        ? (bounds.maxX - bounds.minX) / (bounds.maxY - bounds.minY)
        : 4 / 3;
    const apply = (width: number) => {
      const w = Math.max(MIN_CANVAS_WIDTH, Math.floor(width));
      const maxH = Math.floor(window.innerHeight * MAX_HEIGHT_VH);
      const h = Math.min(maxH, Math.max(240, Math.round(w / aspect)));
      setSize((prev) =>
        prev.width === w && prev.height === h ? prev : { width: w, height: h }
      );
    };
    apply(wrap.clientWidth);
    const ro = new ResizeObserver((entries) => {
      for (const e of entries) apply(e.contentRect.width);
    });
    ro.observe(wrap);
    return () => ro.disconnect();
  }, [bounds]);

  const geometry: CanvasGeometry = useMemo(
    () => ({ width: size.width, height: size.height, padding: CANVAS_PADDING }),
    [size.width, size.height]
  );
  // Bucket points by genre once; the canvas effect + picker both read
  // from this memo.
  const bucketing = useMemo(
    () =>
      bucketByGenre(
        points.map((p) => ({ track_id: p.track_id, x: p.x, y: p.y, genre: p.genre })),
        TOP_GENRE_BUCKETS,
      ),
    [points],
  );
  // Visible-points view used by the hover picker. A hidden bucket
  // shouldn't be hoverable — that would be confusing. In PC mode the
  // legend is hidden so every point stays hoverable.
  const visiblePoints: ScatterPoint[] = useMemo(() => {
    if (colorMode !== "genre") {
      return points.map((p) => ({ track_id: p.track_id, x: p.x, y: p.y }));
    }
    const out: ScatterPoint[] = [];
    for (const b of bucketing.buckets) {
      if (hiddenBuckets.has(b.label)) continue;
      const slice = bucketing.pointsByLabel.get(b.label);
      if (slice) out.push(...slice);
    }
    return out;
  }, [bucketing, hiddenBuckets, colorMode, points]);

  // Min/max of the active PC across the dataset. Recomputed when the
  // user switches PC or the dataset changes — never inside the paint
  // loop. `null` when no point in this projection has a value for the
  // chosen PC (caller paints all dots in the neutral colour).
  const pcRange = useMemo(() => {
    if (colorMode === "genre") return null;
    return rangeOfFiniteValues(points.map((p) => continuousValue(p, colorMode)));
  }, [points, colorMode]);

  // Latent-space neighbours of the currently-hovered point, projected
  // onto our scatter coordinates. `null` when no hover, when the fetch
  // is in flight, or when the response is for a stale hover target
  // (the user moved on before the network resolved). Order is
  // preserved from the backend: ascending cosine distance.
  const neighbourOverlay = useMemo<
    { point: LatentSpacePoint; entry: LatentNeighbourEntry }[] | null
  >(() => {
    if (!hover) return null;
    const data = neighboursQ.data;
    if (!data || data.track_id !== hover.track_id) return null;
    const ids = data.neighbours.map((n) => n.track_id);
    const projected = pickPointsByIds(ids, points);
    // Re-pair: we may have dropped neighbours that aren't in this
    // projection. Walk the projected list and pull the matching entry
    // by id rather than relying on positional alignment.
    const byId = new Map(data.neighbours.map((n) => [n.track_id, n]));
    return projected
      .map((p) => {
        const entry = byId.get(p.track_id);
        return entry ? { point: p, entry } : null;
      })
      .filter((x): x is { point: LatentSpacePoint; entry: LatentNeighbourEntry } => x !== null);
  }, [hover, neighboursQ.data, points]);

  // Paint the canvas whenever points, bounds, or visibility change.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !bounds) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    // High-DPI scale so dots stay crisp on retina screens. The CSS size
    // stays at geometry.width × geometry.height — only the backing store
    // is upscaled. Always reset the transform on size change; otherwise
    // a previously-applied DPR scale would compound after a resize.
    const dpr = window.devicePixelRatio || 1;
    const targetW = geometry.width * dpr;
    const targetH = geometry.height * dpr;
    if (canvas.width !== targetW || canvas.height !== targetH) {
      canvas.width = targetW;
      canvas.height = targetH;
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

    ctx.clearRect(0, 0, geometry.width, geometry.height);
    // Subtle backdrop border so the chart's extent is visible even on
    // sparse projections.
    ctx.strokeStyle = "rgba(255,255,255,0.05)";
    ctx.strokeRect(0.5, 0.5, geometry.width - 1, geometry.height - 1);

    if (colorMode === "genre") {
      // One Path2D per bucket → one fill() per colour. Order matters:
      // we draw Unknown first so named buckets paint on top — the eye
      // follows colour, not grey.
      const drawOrder = [...bucketing.buckets].reverse();
      for (const bucket of drawOrder) {
        if (hiddenBuckets.has(bucket.label)) continue;
        const slice = bucketing.pointsByLabel.get(bucket.label);
        if (!slice || slice.length === 0) continue;
        // Slight alpha so overlapping clusters reveal density.
        ctx.fillStyle = withAlpha(bucket.color, 0.65);
        const path = new Path2D();
        for (const p of slice) {
          const { px, py } = scaleToCanvas(p, bounds, geometry);
          path.moveTo(px + POINT_RADIUS, py);
          path.arc(px, py, POINT_RADIUS, 0, Math.PI * 2);
        }
        ctx.fill(path);
      }
    } else {
      // Continuous-value mode: each point gets its own viridis colour
      // based on its PC value. We sacrifice the bucketed Path2D
      // optimisation here (one fill() per dot) because every dot has
      // a unique fillStyle; the trade is acceptable at our N. Points
      // missing the chosen PC render in a neutral grey to make the
      // absence visually distinct from the gradient endpoints.
      for (const p of points) {
        const v = continuousValue(p, colorMode);
        const fill =
          v === null || pcRange === null
            ? withAlpha("#888888", 0.45)
            : withAlpha(viridis(normalizeInto01(v, pcRange)), 0.75);
        ctx.fillStyle = fill;
        const { px, py } = scaleToCanvas(p, bounds, geometry);
        ctx.beginPath();
        ctx.arc(px, py, POINT_RADIUS, 0, Math.PI * 2);
        ctx.fill();
      }
    }

    // Session paths overlay. Each session draws as a polyline through
    // its events' projected positions, with per-segment stroke width
    // encoding cosine distance in the original CLAP space (UMAP is
    // locally faithful but globally lossy, so the line length carries
    // no real meaning — the width does). Segments through tracks not
    // in this projection are dashed to make the gap visible.
    if (sessions.length > 0) {
      // O(N) lookup table: track_id → projection point. Built once per
      // (points, sessions) change.
      const byTrack = new Map<string, LatentSpacePoint>();
      for (const p of points) byTrack.set(p.track_id, p);
      for (const s of sessions) {
        const hue = sessionHue(s.session_id);
        const stroke = `hsl(${hue}deg 80% 65%)`;
        const events = s.events ?? [];
        const segments = s.segments ?? [];
        // Draw each segment individually so stroke width can vary by
        // segment. A single Path2D with one stroke() would force a
        // uniform width.
        for (let i = 0; i + 1 < events.length; i++) {
          const a = byTrack.get(events[i]!.track_id);
          const b = byTrack.get(events[i + 1]!.track_id);
          if (!a || !b) continue;
          const seg = segments[i];
          const dist = seg ? seg.cosine_distance : null;
          ctx.lineWidth = cosineDistanceToWidth(dist, PATH_WIDTH_RANGE);
          ctx.strokeStyle = stroke;
          ctx.setLineDash(dist === null ? [4, 3] : []);
          const pa = scaleToCanvas(a, bounds, geometry);
          const pb = scaleToCanvas(b, bounds, geometry);
          ctx.beginPath();
          ctx.moveTo(pa.px, pa.py);
          ctx.lineTo(pb.px, pb.py);
          ctx.stroke();
        }
        ctx.setLineDash([]);
        // Anchor as a hollow ring on top of its dot — gives the user a
        // visual "this is where the session started".
        const anchor = byTrack.get(s.anchor_track_id);
        if (anchor) {
          const { px, py } = scaleToCanvas(anchor, bounds, geometry);
          ctx.strokeStyle = stroke;
          ctx.lineWidth = 2;
          ctx.beginPath();
          ctx.arc(px, py, POINT_RADIUS + 4, 0, Math.PI * 2);
          ctx.stroke();
        }
        // Terminal marker — small filled dot on the last event so the
        // direction of travel reads at a glance.
        const last = events[events.length - 1];
        const lastPt = last ? byTrack.get(last.track_id) : undefined;
        if (lastPt) {
          const { px, py } = scaleToCanvas(lastPt, bounds, geometry);
          ctx.fillStyle = stroke;
          ctx.beginPath();
          ctx.arc(px, py, POINT_RADIUS + 1.5, 0, Math.PI * 2);
          ctx.fill();
        }
      }
    }

    // Latent-space neighbours of the hovered point. The whole purpose
    // of this overlay is to *contradict* the 2D layout: a neighbour
    // halfway across the canvas means UMAP couldn't preserve that
    // relationship. Lines from hover → each neighbour make the
    // disparity visible at a glance; longer line == bigger lie.
    if (hover && neighbourOverlay && neighbourOverlay.length > 0) {
      const hoverPx = scaleToCanvas(hover, bounds, geometry);
      const accent = "rgba(245, 158, 11, 0.85)"; // amber, matches hover marker
      // Connecting lines first, so the rings draw on top of them.
      ctx.strokeStyle = accent;
      ctx.lineWidth = 1;
      ctx.setLineDash([3, 3]);
      ctx.beginPath();
      for (const { point } of neighbourOverlay) {
        const target = scaleToCanvas(point, bounds, geometry);
        ctx.moveTo(hoverPx.px, hoverPx.py);
        ctx.lineTo(target.px, target.py);
      }
      ctx.stroke();
      ctx.setLineDash([]);
      // Rings around each neighbour dot. Solid amber outline; the
      // underlying dot keeps its own (genre or PC) colour so the
      // user can still see which cluster it belongs to.
      ctx.strokeStyle = accent;
      ctx.lineWidth = 1.5;
      for (const { point } of neighbourOverlay) {
        const { px, py } = scaleToCanvas(point, bounds, geometry);
        ctx.beginPath();
        ctx.arc(px, py, POINT_RADIUS + 2, 0, Math.PI * 2);
        ctx.stroke();
      }
    }

    // Highlight the hovered point on top of the bulk pass.
    if (hover) {
      const { px, py } = scaleToCanvas(hover, bounds, geometry);
      ctx.fillStyle = "rgba(245, 158, 11, 1)";
      ctx.beginPath();
      ctx.arc(px, py, POINT_RADIUS + 2, 0, Math.PI * 2);
      ctx.fill();
    }
  }, [
    bucketing,
    bounds,
    hover,
    hiddenBuckets,
    geometry,
    points,
    sessions,
    colorMode,
    pcRange,
    neighbourOverlay,
  ]);

  const handlePointerMove = (e: React.PointerEvent<HTMLCanvasElement>) => {
    if (!bounds) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const cursor = { px: e.clientX - rect.left, py: e.clientY - rect.top };
    const picked = pickNearestPoint(
      cursor,
      visiblePoints,
      bounds,
      geometry,
      HOVER_RADIUS
    );
    if (picked?.track_id !== hover?.track_id) {
      // Round-trip back to the original (metadata-bearing) point.
      const full = picked ? points.find((p) => p.track_id === picked.track_id) ?? null : null;
      setHover(full);
    }
  };

  const handlePointerLeave = () => setHover(null);

  const handleClick = async () => {
    if (!hover || playingTrackId === hover.track_id) return;
    setPlayingTrackId(hover.track_id);
    try {
      const track = await getSong(hover.track_id);
      playback.playSingle(track);
    } catch (err) {
      console.error("latent_space: getSong failed", err);
    } finally {
      setPlayingTrackId(null);
    }
  };

  const toggleBucket = (label: string) => {
    setHiddenBuckets((prev) => {
      const next = new Set(prev);
      if (next.has(label)) next.delete(label);
      else next.add(label);
      return next;
    });
  };

  return (
    <div>
      <p className="text-sm" style={{ color: "var(--muted)", marginBottom: "var(--space-2)" }}>
        {points.length} tracks · hover for details · click to play
      </p>
      <div
        style={{
          display: "flex",
          gap: "var(--space-3)",
          alignItems: "flex-start",
          flexWrap: "wrap",
        }}
      >
        {/* flex: 1 lets the canvas column claim every pixel the legend
             doesn't need; minWidth: 0 stops a long-genre legend label
             from preventing the canvas from shrinking on narrow viewports. */}
        <div
          ref={wrapRef}
          style={{ position: "relative", flex: "1 1 0", minWidth: 0 }}
        >
          <canvas
            ref={canvasRef}
            // CSS size matches the measured wrapper width and the
            // bounds-derived height; canvas.width/.height (set in the
            // effect) is the backing-store size for DPR.
            style={{
              width: size.width,
              height: size.height,
              display: "block",
              cursor: hover ? "pointer" : "crosshair",
              background: "var(--surface-2, #0e1116)",
              borderRadius: "var(--radius-2, 4px)",
            }}
            onPointerMove={handlePointerMove}
            onPointerLeave={handlePointerLeave}
            onClick={handleClick}
          />
          {hover && bounds && (
            <HoverTooltip
              point={hover}
              bounds={bounds}
              geometry={geometry}
              loading={playingTrackId === hover.track_id}
              neighbours={
                neighbourOverlay && neighbourOverlay.length > 0
                  ? neighbourOverlay
                  : null
              }
              neighboursLoading={neighboursQ.isFetching && !neighbourOverlay}
            />
          )}
        </div>
        {colorMode === "genre" ? (
          <Legend
            buckets={bucketing.buckets}
            hidden={hiddenBuckets}
            onToggle={toggleBucket}
          />
        ) : (
          <PcGradientLegend mode={colorMode} range={pcRange} />
        )}
      </div>
    </div>
  );
}

// Mix a hex colour `#rrggbb` with an alpha into an rgba() string. The
// canvas API accepts hex but not `#rrggbb + alpha`; rather than carry
// rgba strings through the palette we convert at draw time.
function withAlpha(hex: string, alpha: number): string {
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

function Legend({
  buckets,
  hidden,
  onToggle,
}: {
  buckets: GenreBucket[];
  hidden: Set<string>;
  onToggle: (label: string) => void;
}) {
  if (buckets.length === 0) return null;
  return (
    <ul
      style={{
        listStyle: "none",
        padding: 0,
        margin: 0,
        minWidth: 160,
        fontSize: "0.85em",
      }}
    >
      {buckets.map((b) => {
        const isHidden = hidden.has(b.label);
        return (
          <li
            key={b.label}
            // Buttons-in-a-list would be more semantic; the visual is a
            // colour swatch + label row so a plain <li> with role=button
            // gives keyboard users the right affordance without breaking
            // the layout.
            role="button"
            tabIndex={0}
            onClick={() => onToggle(b.label)}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                onToggle(b.label);
              }
            }}
            style={{
              display: "flex",
              alignItems: "center",
              gap: "var(--space-2, 8px)",
              padding: "2px 4px",
              borderRadius: "var(--radius-1, 2px)",
              cursor: "pointer",
              opacity: isHidden ? 0.4 : 1,
              userSelect: "none",
            }}
          >
            <span
              aria-hidden="true"
              style={{
                width: 10,
                height: 10,
                borderRadius: "50%",
                background: b.color,
                flexShrink: 0,
                // Strike-through when hidden gives a visual cue beyond opacity.
                outline: isHidden ? "1px solid var(--muted)" : "none",
              }}
            />
            <span
              style={{
                textDecoration: isHidden ? "line-through" : "none",
                color: isHidden ? "var(--muted)" : "inherit",
                flex: 1,
                minWidth: 0,
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
              title={b.label}
            >
              {b.label}
            </span>
            <span style={{ color: "var(--muted)", fontVariantNumeric: "tabular-nums" }}>
              {b.count}
            </span>
          </li>
        );
      })}
    </ul>
  );
}

function PcGradientLegend({
  mode,
  range,
}: {
  mode: ColorMode;
  range: { min: number; max: number } | null;
}) {
  if (mode === "genre") return null;
  // Pretty-print: PC modes uppercase to "PC1"; the UMAP-z mode reads
  // as "UMAP z" since underscore-uppercase would be ugly in the chart.
  const label = mode === "umap_z" ? "UMAP z" : mode.toUpperCase();
  if (!range) {
    return (
      <div style={{ minWidth: 160, fontSize: "0.85em" }}>
        <div style={{ color: "var(--muted)" }}>
          {label} not yet populated for this projection. Re-run the reducer
          to compute it.
        </div>
      </div>
    );
  }
  // Build the CSS gradient from the same 5 stops the viridis() helper
  // uses, so the legend matches the canvas exactly.
  const stops = [0, 0.25, 0.5, 0.75, 1].map((t) => viridis(t)).join(", ");
  return (
    <div style={{ minWidth: 160, fontSize: "0.85em" }}>
      <div style={{ color: "var(--muted)", marginBottom: 4 }}>
        {label} (continuous)
      </div>
      <div
        aria-hidden="true"
        style={{
          height: 12,
          background: `linear-gradient(to right, ${stops})`,
          borderRadius: "var(--radius-1, 2px)",
          marginBottom: 4,
        }}
      />
      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          color: "var(--muted)",
          fontVariantNumeric: "tabular-nums",
        }}
      >
        <span>{range.min.toFixed(2)}</span>
        <span>{range.max.toFixed(2)}</span>
      </div>
    </div>
  );
}

function HoverTooltip({
  point,
  bounds,
  geometry,
  loading,
  neighbours,
  neighboursLoading,
}: {
  point: LatentSpacePoint;
  bounds: DataBounds;
  geometry: CanvasGeometry;
  loading: boolean;
  /** Latent-space neighbours of `point`, already paired with their
   *  projected scatter point. Null when the fetch is in flight or
   *  there are no neighbours to draw. */
  neighbours: { point: LatentSpacePoint; entry: LatentNeighbourEntry }[] | null;
  /** True while the neighbour fetch is in flight for this point. */
  neighboursLoading: boolean;
}) {
  const { px, py } = scaleToCanvas(point, bounds, geometry);
  // Anchor near the cursor without escaping the canvas — flip to the
  // left of the point if we're in the right half.
  const flipX = px > geometry.width / 2;
  const flipY = py > geometry.height - 80;
  const style: React.CSSProperties = {
    position: "absolute",
    left: flipX ? undefined : px + 10,
    right: flipX ? geometry.width - px + 10 : undefined,
    top: flipY ? undefined : py + 10,
    bottom: flipY ? geometry.height - py + 10 : undefined,
    pointerEvents: "none",
    background: "var(--surface-3, #1a1f2c)",
    padding: "var(--space-2, 8px) var(--space-3, 12px)",
    borderRadius: "var(--radius-2, 4px)",
    border: "1px solid var(--border, #2a2f3a)",
    fontSize: "0.85em",
    maxWidth: 280,
    zIndex: 2,
  };
  return (
    <div style={style}>
      <div style={{ fontWeight: 500 }}>{point.title ?? <code>{point.track_id}</code>}</div>
      {point.artist && <div style={{ color: "var(--muted)" }}>{point.artist}</div>}
      {point.album && (
        <div style={{ color: "var(--muted)", fontSize: "0.9em" }}>{point.album}</div>
      )}
      {loading && (
        <div style={{ color: "var(--muted)", fontStyle: "italic", marginTop: 4 }}>
          loading…
        </div>
      )}
      {neighboursLoading && (
        <div style={{ color: "var(--muted)", fontStyle: "italic", marginTop: 4 }}>
          fetching neighbours…
        </div>
      )}
      {neighbours && neighbours.length > 0 && (
        <div style={{ marginTop: 6, paddingTop: 6, borderTop: "1px solid var(--border, #2a2f3a)" }}>
          <div style={{ color: "var(--muted)", fontSize: "0.85em" }}>
            {neighbours.length} nearest in latent space
          </div>
          {neighbours.slice(0, 5).map(({ point: np, entry }) => (
            <div
              key={np.track_id}
              style={{
                display: "flex",
                justifyContent: "space-between",
                gap: 12,
                fontSize: "0.85em",
              }}
            >
              <span
                style={{
                  whiteSpace: "nowrap",
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                }}
              >
                {np.title ?? np.track_id}
              </span>
              <span style={{ color: "var(--muted)", fontVariantNumeric: "tabular-nums" }}>
                {entry.cosine_distance.toFixed(3)}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// Small hover popover for inline explanations next to controls. Pure
// React state — no portal — so the popover is clipped by ancestor
// `overflow: hidden`; that's fine here because the toolbar row sits at
// the top of the page with room to expand downwards. Keyboard users
// get the same content via the underlying button's `aria-label` and
// the popover's `role="tooltip"` association.
function InfoTooltip({ children }: { children: React.ReactNode }) {
  const [open, setOpen] = useState(false);
  return (
    <span
      style={{ position: "relative", display: "inline-flex" }}
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
      onFocus={() => setOpen(true)}
      onBlur={() => setOpen(false)}
    >
      <button
        type="button"
        aria-label="What does colour-by encode?"
        aria-expanded={open}
        style={{
          width: 16,
          height: 16,
          borderRadius: "50%",
          border: "1px solid var(--border, #2a2f3a)",
          background: "transparent",
          color: "var(--muted)",
          fontSize: 11,
          lineHeight: "14px",
          padding: 0,
          cursor: "help",
          fontFamily: "var(--font-serif, serif)",
          fontStyle: "italic",
        }}
      >
        i
      </button>
      {open && (
        <div
          role="tooltip"
          style={{
            position: "absolute",
            top: "calc(100% + 6px)",
            left: 0,
            zIndex: 3,
            background: "var(--surface-3, #1a1f2c)",
            padding: "var(--space-3, 12px)",
            borderRadius: "var(--radius-2, 4px)",
            border: "1px solid var(--border, #2a2f3a)",
            fontSize: "0.85em",
            lineHeight: 1.45,
            width: 340,
            color: "var(--text, #d8dbe2)",
            whiteSpace: "normal",
            pointerEvents: "none",
          }}
        >
          {children}
        </div>
      )}
    </span>
  );
}
