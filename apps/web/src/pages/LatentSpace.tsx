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
import { useEffect, useMemo, useRef, useState } from "react";

import { getSong } from "../api/client";
import {
  fetchRecommendLatentSpace,
  fetchRecommendSessions,
  LatentSpacePoint,
  SessionItem,
} from "../api/diagnostics";
import { Layout } from "../components/Layout";
import { Link } from "../router";
import { usePlayback } from "../sync/usePlayback";
import {
  bucketByGenre,
  computeBounds,
  cosineDistanceToWidth,
  pickNearestPoint,
  scaleToCanvas,
  sessionHue,
  type DataBounds,
  type CanvasGeometry,
  type GenreBucket,
  type ScatterPoint,
} from "./latentSpace";

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
  const [selectedProj, setSelectedProj] = useState<string | undefined>();
  // Dropdown state for the session overlay. Defaults to "none" — the
  // scatter is useful without a path, and fetching event-bundled
  // sessions has a real cost on the gateway side.
  const [sessionPick, setSessionPick] = useState<string>(SESSION_NONE);

  const { data, error, isLoading } = useQuery({
    queryKey: ["diag", "latent_space", selectedProj ?? null],
    queryFn: () =>
      fetchRecommendLatentSpace(
        selectedProj ? { projVersion: selectedProj } : {}
      ),
    // The reducer is a batch job — projection rarely changes during a
    // session. 30s keeps it fresh enough for "I just re-ran the reducer
    // in another terminal" without churn.
    refetchInterval: 30_000,
  });

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
    // Sessions change slowly; same refetch cadence as the scatter.
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
            <ProjectionPicker
              versions={data.versions}
              modelVersion={data.model_version}
              selectedProj={data.proj_version}
              onSelect={(pv) => setSelectedProj(pv)}
            />
            <SessionPicker
              value={sessionPick}
              sessions={sessionsQ.data?.items ?? []}
              loading={wantSessions && sessionsQ.isFetching}
              onChange={setSessionPick}
            />
            {data.points.length === 0 ? (
              <EmptyHint
                hasProjection={data.proj_version !== null}
                modelVersion={data.model_version}
              />
            ) : (
              <ScatterCanvas
                points={data.points}
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
          style={{ fontFamily: "var(--font-mono)" }}
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

function ProjectionPicker({
  versions,
  modelVersion,
  selectedProj,
  onSelect,
}: {
  versions: { proj_version: string; point_count: number; created_at_ms: number }[];
  modelVersion: string;
  selectedProj: string | null;
  onSelect: (proj: string) => void;
}) {
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
        proj_version&nbsp;
        <select
          value={selectedProj ?? ""}
          disabled={versions.length === 0}
          onChange={(e) => onSelect(e.target.value)}
          style={{ fontFamily: "var(--font-mono)" }}
        >
          {versions.length === 0 && <option value="">— none —</option>}
          {versions.map((v) => (
            <option key={v.proj_version} value={v.proj_version}>
              {v.proj_version} ({v.point_count} pts)
            </option>
          ))}
        </select>
      </label>
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
}: {
  hasProjection: boolean;
  modelVersion: string;
}) {
  if (!hasProjection) {
    return (
      <p className="text-sm" style={{ color: "var(--muted)" }}>
        no projection has been written yet for{" "}
        <code>{modelVersion}</code>. Run{" "}
        <code>
          uv run --extra reduce python -m embedder.reduce --db &lt;path&gt; --model-version{" "}
          {modelVersion}
        </code>{" "}
        in <code>services/embedder/</code> to populate it.
      </p>
    );
  }
  // Projection exists for this model but the queried proj_version has
  // zero points — usually because the user picked a stale version that
  // was rebuilt under a new name.
  return (
    <p className="text-sm" style={{ color: "var(--muted)" }}>
      this projection is empty.
    </p>
  );
}

function ScatterCanvas({
  points,
  sessions,
}: {
  points: LatentSpacePoint[];
  sessions: readonly SessionItem[];
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const [hover, setHover] = useState<LatentSpacePoint | null>(null);
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
  // shouldn't be hoverable — that would be confusing.
  const visiblePoints: ScatterPoint[] = useMemo(() => {
    const out: ScatterPoint[] = [];
    for (const b of bucketing.buckets) {
      if (hiddenBuckets.has(b.label)) continue;
      const slice = bucketing.pointsByLabel.get(b.label);
      if (slice) out.push(...slice);
    }
    return out;
  }, [bucketing, hiddenBuckets]);

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

    // One Path2D per bucket → one fill() per colour. Order matters: we
    // draw Unknown first so named buckets paint on top — the eye
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

    // Highlight the hovered point on top of the bulk pass.
    if (hover) {
      const { px, py } = scaleToCanvas(hover, bounds, geometry);
      ctx.fillStyle = "rgba(245, 158, 11, 1)";
      ctx.beginPath();
      ctx.arc(px, py, POINT_RADIUS + 2, 0, Math.PI * 2);
      ctx.fill();
    }
  }, [bucketing, bounds, hover, hiddenBuckets, geometry, points, sessions]);

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
            />
          )}
        </div>
        <Legend
          buckets={bucketing.buckets}
          hidden={hiddenBuckets}
          onToggle={toggleBucket}
        />
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

function HoverTooltip({
  point,
  bounds,
  geometry,
  loading,
}: {
  point: LatentSpacePoint;
  bounds: DataBounds;
  geometry: CanvasGeometry;
  loading: boolean;
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
    </div>
  );
}
