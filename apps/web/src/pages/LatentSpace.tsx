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
import { fetchRecommendLatentSpace, LatentSpacePoint } from "../api/diagnostics";
import { Layout } from "../components/Layout";
import { Link } from "../router";
import { usePlayback } from "../sync/usePlayback";
import {
  bucketByGenre,
  computeBounds,
  pickNearestPoint,
  scaleToCanvas,
  type DataBounds,
  type CanvasGeometry,
  type GenreBucket,
  type ScatterPoint,
} from "./latentSpace";

const CANVAS_WIDTH = 720;
const CANVAS_HEIGHT = 540;
const CANVAS_PADDING = 24;
const POINT_RADIUS = 2.5;
// Slightly larger than POINT_RADIUS so the hover target is forgiving
// — picking a 2.5px dot exactly is annoying on a touchpad.
const HOVER_RADIUS = 8;
// Cap on named-genre buckets in the legend. Subsonic libraries
// routinely have 50+ rare tags; we keep the top 10 (matches the
// palette size) and roll the rest into 'Other'.
const TOP_GENRE_BUCKETS = 10;

const CANVAS_GEOMETRY: CanvasGeometry = {
  width: CANVAS_WIDTH,
  height: CANVAS_HEIGHT,
  padding: CANVAS_PADDING,
};

export function LatentSpace() {
  const [selectedProj, setSelectedProj] = useState<string | undefined>();

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
            {data.points.length === 0 ? (
              <EmptyHint
                hasProjection={data.proj_version !== null}
                modelVersion={data.model_version}
              />
            ) : (
              <ScatterCanvas points={data.points} />
            )}
          </>
        )}

        {isLoading && !data && <p className="text-sm">loading projection…</p>}
      </div>
    </Layout>
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

function ScatterCanvas({ points }: { points: LatentSpacePoint[] }) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [hover, setHover] = useState<LatentSpacePoint | null>(null);
  const playback = usePlayback();
  // Resolving track_id → Track via Subsonic getSong is a separate
  // network round-trip; we surface a transient "loading" state so a
  // mis-click can be acknowledged.
  const [playingTrackId, setPlayingTrackId] = useState<string | null>(null);
  // Legend filter — click a bucket to toggle its visibility. Stored as
  // a Set so the canvas + picker can both consult it cheaply.
  const [hiddenBuckets, setHiddenBuckets] = useState<Set<string>>(new Set());

  const bounds: DataBounds | null = useMemo(() => computeBounds(points), [points]);
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
    // stays at CANVAS_WIDTH × CANVAS_HEIGHT — only the backing store is
    // upscaled.
    const dpr = window.devicePixelRatio || 1;
    if (canvas.width !== CANVAS_WIDTH * dpr) {
      canvas.width = CANVAS_WIDTH * dpr;
      canvas.height = CANVAS_HEIGHT * dpr;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    }

    ctx.clearRect(0, 0, CANVAS_WIDTH, CANVAS_HEIGHT);
    // Subtle backdrop border so the chart's extent is visible even on
    // sparse projections.
    ctx.strokeStyle = "rgba(255,255,255,0.05)";
    ctx.strokeRect(0.5, 0.5, CANVAS_WIDTH - 1, CANVAS_HEIGHT - 1);

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
        const { px, py } = scaleToCanvas(p, bounds, CANVAS_GEOMETRY);
        path.moveTo(px + POINT_RADIUS, py);
        path.arc(px, py, POINT_RADIUS, 0, Math.PI * 2);
      }
      ctx.fill(path);
    }

    // Highlight the hovered point on top of the bulk pass.
    if (hover) {
      const { px, py } = scaleToCanvas(hover, bounds, CANVAS_GEOMETRY);
      ctx.fillStyle = "rgba(245, 158, 11, 1)";
      ctx.beginPath();
      ctx.arc(px, py, POINT_RADIUS + 2, 0, Math.PI * 2);
      ctx.fill();
    }
  }, [bucketing, bounds, hover, hiddenBuckets]);

  const handlePointerMove = (e: React.PointerEvent<HTMLCanvasElement>) => {
    if (!bounds) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const cursor = { px: e.clientX - rect.left, py: e.clientY - rect.top };
    const picked = pickNearestPoint(
      cursor,
      visiblePoints,
      bounds,
      CANVAS_GEOMETRY,
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
        <div style={{ position: "relative", width: CANVAS_WIDTH, maxWidth: "100%" }}>
          <canvas
            ref={canvasRef}
            // CSS size is the logical size; canvas.width/.height (set in
            // the effect) is the backing-store size for DPR.
            style={{
              width: CANVAS_WIDTH,
              height: CANVAS_HEIGHT,
              maxWidth: "100%",
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
            <HoverTooltip point={hover} bounds={bounds} loading={playingTrackId === hover.track_id} />
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
  loading,
}: {
  point: LatentSpacePoint;
  bounds: DataBounds;
  loading: boolean;
}) {
  const { px, py } = scaleToCanvas(point, bounds, CANVAS_GEOMETRY);
  // Anchor near the cursor without escaping the canvas — flip to the
  // left of the point if we're in the right half.
  const flipX = px > CANVAS_WIDTH / 2;
  const flipY = py > CANVAS_HEIGHT - 80;
  const style: React.CSSProperties = {
    position: "absolute",
    left: flipX ? undefined : px + 10,
    right: flipX ? CANVAS_WIDTH - px + 10 : undefined,
    top: flipY ? undefined : py + 10,
    bottom: flipY ? CANVAS_HEIGHT - py + 10 : undefined,
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
