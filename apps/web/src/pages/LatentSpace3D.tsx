// 3-D scatter view for the latent-space page. Rendered with
// react-three-fiber when colourMode is "umap_z" so the third UMAP axis
// becomes a real spatial dimension instead of a 2-D-with-z-as-colour
// flatten. OrbitControls give drag-to-rotate, scroll-to-zoom,
// right-drag-to-pan.
//
// Performance shape: one buffer-geometry with two attributes (position
// + colour) → a single draw call for the whole cloud. Hover picking
// is the three.js raycaster against `<points>` with a forgiving
// threshold; click plays the hovered track.

import { useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Canvas, ThreeEvent, useThree } from "@react-three/fiber";
import { CanvasTexture, Texture } from "three";
import { OrbitControls } from "three/examples/jsm/controls/OrbitControls.js";
import { LineMaterial } from "three/examples/jsm/lines/LineMaterial.js";
import { LineSegments2 } from "three/examples/jsm/lines/LineSegments2.js";
import { LineSegmentsGeometry } from "three/examples/jsm/lines/LineSegmentsGeometry.js";

import { getSong } from "../api/client";
import {
  fetchRecommendLatentNeighbours,
  LatentSpacePoint,
  SessionItem,
} from "../api/diagnostics";
import { usePlayback } from "../sync/usePlayback";

import {
  compute3dBounds,
  normalizeInto01,
  normalizeTo3dCube,
  rangeOfFiniteValues,
  sessionHue,
  viridis,
} from "./latentSpace";

const POINT_SIZE = 0.04;

/// Soft-edged white disc on transparent background. Used as the `map`
/// on every PointsMaterial so dots render as circles instead of the
/// default square sprite. Built lazily on first use (canvas operations
/// would fail at module load in a non-browser test runner), then
/// shared across all materials — three.js textures are immutable once
/// uploaded, so one instance is enough.
let CIRCLE_TEXTURE: Texture | null = null;
function getCircleTexture(): Texture {
  if (CIRCLE_TEXTURE) return CIRCLE_TEXTURE;
  const size = 64;
  const canvas = document.createElement("canvas");
  canvas.width = size;
  canvas.height = size;
  const ctx = canvas.getContext("2d")!;
  // Radial gradient: solid white at the centre, fully transparent at
  // the edge. Anti-aliases the disc without needing MSAA on the
  // points material itself.
  const grad = ctx.createRadialGradient(
    size / 2,
    size / 2,
    0,
    size / 2,
    size / 2,
    size / 2,
  );
  grad.addColorStop(0, "rgba(255,255,255,1)");
  grad.addColorStop(0.7, "rgba(255,255,255,1)");
  grad.addColorStop(1, "rgba(255,255,255,0)");
  ctx.fillStyle = grad;
  ctx.fillRect(0, 0, size, size);
  CIRCLE_TEXTURE = new CanvasTexture(canvas);
  CIRCLE_TEXTURE.needsUpdate = true;
  return CIRCLE_TEXTURE;
}
const HOVER_POINT_SIZE = 0.08;
// Raycaster threshold in world units. Empirical: ~1.5× point radius
// gives a forgiving but not sticky hit area. Tightened further and you
// have to nail the pixel; loosened further and adjacent dots fight for
// the hover.
const RAY_THRESHOLD = 0.04;
// Camera framing: 2.5 puts the unit cube comfortably in the middle of
// the viewport with the default 50° FOV. Lower → closer / dot-clipping
// risk; higher → wasted screen real estate.
const CAMERA_DISTANCE = 2.5;

export function LatentSpace3D({
  points,
  sessions = [],
}: {
  points: readonly LatentSpacePoint[];
  /** Sessions the user has chosen to overlay (none by default). Each
   *  becomes a coloured polyline through its events' 3-D positions,
   *  with anchor + terminal markers. Empty array = no overlay. */
  sessions?: readonly SessionItem[];
}) {
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const [size, setSize] = useState({ width: 800, height: 600 });
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);
  // Tracked separately from hoverIdx because the tooltip anchors at the
  // cursor position, not the projected dot position — keeps the popover
  // off-cluster and avoids reprojecting per frame.
  const [cursor, setCursor] = useState<{ x: number; y: number } | null>(null);

  useEffect(() => {
    const wrap = wrapRef.current;
    if (!wrap) return;
    const apply = (width: number) => {
      const w = Math.max(320, Math.floor(width));
      // Square-ish aspect: 3-D layouts have no inherent x/y ratio (the
      // unit cube is symmetric), so a 4:3 frame works for everything.
      const h = Math.min(Math.floor(window.innerHeight * 0.7), Math.round(w * 0.75));
      setSize((prev) => (prev.width === w && prev.height === h ? prev : { width: w, height: h }));
    };
    apply(wrap.clientWidth);
    const ro = new ResizeObserver((entries) => {
      for (const e of entries) apply(e.contentRect.width);
    });
    ro.observe(wrap);
    return () => ro.disconnect();
  }, []);

  // Pre-compute the float32 position / colour arrays. Recomputed only
  // when points change — orbit / hover should never trigger a re-upload
  // (positions are static for a given dataset).
  const { positions, colors } = useMemo(() => buildBuffers(points), [points]);

  const hover = hoverIdx !== null ? points[hoverIdx] ?? null : null;
  const [debouncedHoverId, setDebouncedHoverId] = useState<string | null>(null);
  useEffect(() => {
    const id = hover?.track_id ?? null;
    if (id === debouncedHoverId) return;
    const handle = window.setTimeout(() => setDebouncedHoverId(id), 150);
    return () => window.clearTimeout(handle);
  }, [hover, debouncedHoverId]);
  // Latent-space neighbours of the hovered point. Same fetch as the
  // 2-D view; k matches so the two visualisations stay comparable when
  // the user flips colour modes. The neighbour overlay below draws
  // lines from hover → each neighbour, materialising the CLAP-space
  // adjacency that the UMAP layout can't always preserve.
  const neighboursQ = useQuery({
    queryKey: ["latent-neighbours", debouncedHoverId],
    queryFn: () =>
      fetchRecommendLatentNeighbours({ trackId: debouncedHoverId!, k: 10 }),
    enabled: !!debouncedHoverId,
    staleTime: 60 * 60 * 1000,
  });

  // Map track_id → buffer index so the neighbour overlay can find the
  // world-space positions of points returned by the gateway. Rebuilt
  // only when the dataset changes (not on hover).
  const indexByTrackId = useMemo(() => {
    const m = new Map<string, number>();
    for (let i = 0; i < points.length; i++) m.set(points[i]!.track_id, i);
    return m;
  }, [points]);

  // Pre-compute per-session geometry: line segments through events,
  // anchor + terminal positions, and a stable colour per session. Only
  // tracks present in this projection contribute — missing tracks
  // break a session into multiple visible runs.
  const sessionGeometries = useMemo(
    () => buildSessionGeometries(sessions, points, indexByTrackId, positions),
    [sessions, points, indexByTrackId, positions],
  );

  // Float32 arrays for the neighbour overlay's `<lineSegments>` and
  // marker spheres. `null` when no usable overlay (no hover, stale
  // fetch, or every neighbour absent from this projection).
  const neighbourOverlay = useMemo(() => {
    if (!hover) return null;
    const data = neighboursQ.data;
    if (!data || data.track_id !== hover.track_id) return null;
    const hoverIdxInBuf = indexByTrackId.get(hover.track_id);
    if (hoverIdxInBuf === undefined) return null;
    const hx = positions[hoverIdxInBuf * 3] ?? 0;
    const hy = positions[hoverIdxInBuf * 3 + 1] ?? 0;
    const hz = positions[hoverIdxInBuf * 3 + 2] ?? 0;
    const matched: number[] = [];
    for (const n of data.neighbours) {
      const idx = indexByTrackId.get(n.track_id);
      if (idx !== undefined) matched.push(idx);
    }
    if (matched.length === 0) return null;
    // Each line segment needs two consecutive vertices in the buffer.
    const lineVerts = new Float32Array(matched.length * 6);
    const markerVerts = new Float32Array(matched.length * 3);
    for (let i = 0; i < matched.length; i++) {
      const idx = matched[i]!;
      const nx = positions[idx * 3] ?? 0;
      const ny = positions[idx * 3 + 1] ?? 0;
      const nz = positions[idx * 3 + 2] ?? 0;
      lineVerts[i * 6] = hx;
      lineVerts[i * 6 + 1] = hy;
      lineVerts[i * 6 + 2] = hz;
      lineVerts[i * 6 + 3] = nx;
      lineVerts[i * 6 + 4] = ny;
      lineVerts[i * 6 + 5] = nz;
      markerVerts[i * 3] = nx;
      markerVerts[i * 3 + 1] = ny;
      markerVerts[i * 3 + 2] = nz;
    }
    return { lineVerts, markerVerts };
  }, [hover, neighboursQ.data, indexByTrackId, positions]);

  const playback = usePlayback();
  const [playingTrackId, setPlayingTrackId] = useState<string | null>(null);
  const handleClick = async () => {
    if (!hover || playingTrackId === hover.track_id) return;
    setPlayingTrackId(hover.track_id);
    try {
      const track = await getSong(hover.track_id);
      playback.playSingle(track);
    } catch (err) {
      console.error("latent_space_3d: getSong failed", err);
    } finally {
      setPlayingTrackId(null);
    }
  };

  return (
    <div>
      <p className="text-sm" style={{ color: "var(--muted)", marginBottom: "var(--space-2)" }}>
        {points.length} tracks · drag to rotate · scroll to zoom · click to play
      </p>
      <div
        ref={wrapRef}
        style={{ position: "relative", flex: "1 1 0", minWidth: 0 }}
      >
        <div
          style={{
            width: size.width,
            height: size.height,
            background: "var(--surface-2, #0e1116)",
            borderRadius: "var(--radius-2, 4px)",
            cursor: hover ? "pointer" : "grab",
          }}
          onClick={handleClick}
          onPointerMove={(e) => setCursor({ x: e.clientX, y: e.clientY })}
          onPointerLeave={() => {
            setHoverIdx(null);
            setCursor(null);
          }}
        >
          <Canvas
            camera={{ position: [CAMERA_DISTANCE, CAMERA_DISTANCE, CAMERA_DISTANCE], fov: 50 }}
            // Transparent background so the wrapper div's colour shows
            // through. Saves one extra background-clear per frame.
            gl={{ antialias: true, alpha: true }}
          >
            <Controls />
            <RaycasterTuning />
            {/* No lights needed — pointsMaterial isn't a lit material. */}
            <PointsCloud
              positions={positions}
              colors={colors}
              onHover={setHoverIdx}
            />
            <HoverMarker positions={positions} index={hoverIdx} />
            {neighbourOverlay && (
              <NeighbourOverlay
                lineVerts={neighbourOverlay.lineVerts}
                markerVerts={neighbourOverlay.markerVerts}
              />
            )}
            {sessionGeometries.map((g) => (
              <SessionPath key={g.session_id} geometry={g} />
            ))}
            <SceneAxes />
          </Canvas>
        </div>
        {hover && cursor && (
          <HoverTooltip
            point={hover}
            cursor={cursor}
            loading={playingTrackId === hover.track_id}
            neighbours={
              neighboursQ.data && neighboursQ.data.track_id === hover.track_id
                ? neighboursQ.data.neighbours
                : null
            }
            neighboursLoading={neighboursQ.isFetching && !neighboursQ.data}
            wrapRef={wrapRef}
          />
        )}
      </div>
    </div>
  );
}

/// Build the position / colour Float32Arrays once per dataset. Position
/// space is `[-1, 1]^3`; colour comes from a viridis sample on the
/// normalised z. Points missing z or any of x/y are mapped to a neutral
/// grey at the cube centre — they still draw so the count matches the
/// header but they don't pollute the layout.
function buildBuffers(points: readonly LatentSpacePoint[]): {
  positions: Float32Array;
  colors: Float32Array;
} {
  const bounds = compute3dBounds(
    points
      .filter((p) => p.z !== null)
      .map((p) => ({ track_id: p.track_id, x: p.x, y: p.y, z: p.z as number })),
  );
  const zRange = rangeOfFiniteValues(points.map((p) => p.z));
  const positions = new Float32Array(points.length * 3);
  const colors = new Float32Array(points.length * 3);
  for (let i = 0; i < points.length; i++) {
    const p = points[i]!;
    if (p.z === null || bounds === null) {
      positions[i * 3] = 0;
      positions[i * 3 + 1] = 0;
      positions[i * 3 + 2] = 0;
      colors[i * 3] = 0.5;
      colors[i * 3 + 1] = 0.5;
      colors[i * 3 + 2] = 0.5;
      continue;
    }
    const n = normalizeTo3dCube({ x: p.x, y: p.y, z: p.z }, bounds);
    positions[i * 3] = n.x;
    positions[i * 3 + 1] = n.y;
    positions[i * 3 + 2] = n.z;
    const hex =
      zRange === null ? "#888888" : viridis(normalizeInto01(p.z, zRange));
    const c = hexToLinearRgb(hex);
    colors[i * 3] = c.r;
    colors[i * 3 + 1] = c.g;
    colors[i * 3 + 2] = c.b;
  }
  return { positions, colors };
}

function PointsCloud({
  positions,
  colors,
  onHover,
}: {
  positions: Float32Array;
  colors: Float32Array;
  onHover: (idx: number | null) => void;
}) {
  // R3F's onPointerMove on `<points>` fires per-frame while the cursor
  // is over a point. `event.index` is the buffer index of the hit
  // vertex — we pass it straight up so the parent can resolve the
  // LatentSpacePoint metadata.
  const handleMove = (e: ThreeEvent<PointerEvent>) => {
    if (typeof e.index === "number") onHover(e.index);
  };
  const handleOut = () => onHover(null);
  return (
    <points onPointerMove={handleMove} onPointerOut={handleOut}>
      <bufferGeometry>
        <bufferAttribute
          attach="attributes-position"
          args={[positions, 3]}
        />
        <bufferAttribute
          attach="attributes-color"
          args={[colors, 3]}
        />
      </bufferGeometry>
      <pointsMaterial
        size={POINT_SIZE}
        vertexColors
        sizeAttenuation
        transparent
        opacity={0.85}
        map={getCircleTexture()}
        // Discard pixels below ~30% alpha so the soft-edged disc's
        // outer halo doesn't accumulate when many points overlap.
        // Without this you get a foggy square around every cluster.
        alphaTest={0.3}
        depthWrite={false}
      />
    </points>
  );
}

/// Highlight ring on the hovered point. Rendered as a slightly-larger
/// single-vertex points layer at the same position, in amber to match
/// the 2-D view's hover marker. Hidden when nothing is hovered.
function HoverMarker({
  positions,
  index,
}: {
  positions: Float32Array;
  index: number | null;
}) {
  if (index === null) return null;
  const x = positions[index * 3] ?? 0;
  const y = positions[index * 3 + 1] ?? 0;
  const z = positions[index * 3 + 2] ?? 0;
  return (
    <mesh position={[x, y, z]}>
      <sphereGeometry args={[HOVER_POINT_SIZE / 2, 16, 16]} />
      <meshBasicMaterial color="#f59e0b" />
    </mesh>
  );
}

/// Lines from the hovered point to each neighbour, plus a small amber
/// sphere on every neighbour position. The line geometry uses a single
/// `<lineSegments>` (one draw call) and a `<points>` layer for the
/// markers — sphere meshes per neighbour would balloon the draw count.
/// Colour matches the 2-D view's amber accent for visual continuity.
function NeighbourOverlay({
  lineVerts,
  markerVerts,
}: {
  lineVerts: Float32Array;
  markerVerts: Float32Array;
}) {
  // R3F rebuilds buffer-attributes whenever the `args` tuple's identity
  // changes — the parent useMemo allocates a new Float32Array per hover
  // target, so this works without explicit React keys.
  return (
    <>
      <lineSegments>
        <bufferGeometry>
          <bufferAttribute
            attach="attributes-position"
            args={[lineVerts, 3]}
          />
        </bufferGeometry>
        <lineBasicMaterial color="#f59e0b" transparent opacity={0.7} />
      </lineSegments>
      <points>
        <bufferGeometry>
          <bufferAttribute
            attach="attributes-position"
            args={[markerVerts, 3]}
          />
        </bufferGeometry>
        <pointsMaterial
          color="#f59e0b"
          size={POINT_SIZE * 1.8}
          sizeAttenuation
          transparent
          opacity={0.9}
          map={getCircleTexture()}
          alphaTest={0.3}
          depthWrite={false}
        />
      </points>
    </>
  );
}

/// Thick line-segments using three.js's `LineSegments2` from
/// `examples/jsm/lines`. The vanilla `lineBasicMaterial` ignores
/// `linewidth > 1` on every WebGL driver (the spec lets implementations
/// clamp to 1), which made session paths nearly invisible in the
/// 3-D view. LineSegments2 renders lines as meshed quad strips and so
/// honours real pixel widths.
///
/// Resolution must be kept in sync with the canvas size — the material
/// uses it to convert "pixel width" → world-space quad size each
/// frame. We subscribe to `useThree().size` for that. On unmount we
/// dispose the geometry + material to free GPU memory (R3F doesn't do
/// this automatically for objects we new'd ourselves).
function ThickLineSegments({
  positions,
  color,
  lineWidth,
}: {
  positions: Float32Array;
  color: string;
  lineWidth: number;
}) {
  const { size } = useThree();
  const line = useMemo(() => {
    const geom = new LineSegmentsGeometry();
    geom.setPositions(positions as unknown as number[]);
    const mat = new LineMaterial({
      color,
      linewidth: lineWidth,
      worldUnits: false,
      transparent: true,
      opacity: 1,
    });
    mat.resolution.set(size.width, size.height);
    return new LineSegments2(geom, mat);
    // Disposal handled in the effect below — we want the cleanup to
    // see the *previous* `line` instance, which `useEffect`'s closure
    // captures correctly. A bare `useMemo` would leak on every change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [positions, color, lineWidth]);

  useEffect(() => {
    line.material.resolution.set(size.width, size.height);
  }, [line, size.width, size.height]);

  useEffect(() => {
    return () => {
      line.geometry.dispose();
      line.material.dispose();
    };
  }, [line]);

  return <primitive object={line} />;
}

/// Per-session geometry: a single contiguous polyline through every
/// event that has a position in this projection. Missing tracks break
/// the path — we draw each "run" as its own `<lineSegments>` so absent
/// segments are visually-blank rather than misleading straight lines
/// across a gap. The 2-D view uses dashed strokes for the same idea;
/// three.js's stable-across-drivers options for dashes are heavier
/// (`Line2`/`LineMaterial`), so the discontinuity is enough here.
interface SessionGeometry {
  session_id: string;
  color: string;
  /** Flat `[s0, e0, s1, e1, ...]` for `<lineSegments>`. May be empty if
   *  this session has fewer than two projected events. */
  lineVerts: Float32Array;
  /** World position of the anchor event, or null if not projected. */
  anchor: [number, number, number] | null;
  /** World position of the last event, or null if not projected. */
  terminal: [number, number, number] | null;
}

function buildSessionGeometries(
  sessions: readonly SessionItem[],
  points: readonly LatentSpacePoint[],
  indexByTrackId: ReadonlyMap<string, number>,
  positions: Float32Array,
): SessionGeometry[] {
  if (sessions.length === 0 || points.length === 0) return [];
  const lookup = (trackId: string): [number, number, number] | null => {
    const idx = indexByTrackId.get(trackId);
    if (idx === undefined) return null;
    return [
      positions[idx * 3] ?? 0,
      positions[idx * 3 + 1] ?? 0,
      positions[idx * 3 + 2] ?? 0,
    ];
  };
  return sessions.map((s) => {
    const hue = sessionHue(s.session_id);
    const color = `hsl(${hue}, 80%, 65%)`;
    const events = s.events ?? [];
    // Walk consecutive event pairs; emit a segment iff both endpoints
    // are projected. Missing tracks split the path naturally.
    const segs: number[] = [];
    for (let i = 0; i + 1 < events.length; i++) {
      const a = lookup(events[i]!.track_id);
      const b = lookup(events[i + 1]!.track_id);
      if (!a || !b) continue;
      segs.push(a[0], a[1], a[2], b[0], b[1], b[2]);
    }
    const last = events.at(-1);
    return {
      session_id: s.session_id,
      color,
      lineVerts: new Float32Array(segs),
      anchor: lookup(s.anchor_track_id),
      terminal: last ? lookup(last.track_id) : null,
    };
  });
}

/// One session in the scene. Visual encoding:
///   * Polyline through projected events (one draw call).
///   * Wireframe sphere on the anchor — only for multi-event sessions
///     where there's a path to anchor. Single-event sessions collapse
///     to a single small filled dot instead (otherwise the wireframe
///     dominates the cloud — it's much bigger than the underlying
///     point, and at length=1 the line is empty so it carries no
///     useful information about a "starting position").
///   * Small filled sphere on the terminal — direction-of-travel hint.
function SessionPath({ geometry }: { geometry: SessionGeometry }) {
  const hasLine = geometry.lineVerts.length > 0;

  // Length-1 sessions: no path, anchor == terminal. Render a single
  // small filled dot so the session is *visible* but doesn't drown out
  // the multi-event sessions whose paths are the real story. A sphere
  // of radius r has diameter 2r — pointsMaterial's `size` is roughly
  // diameter, so radius = POINT_SIZE/2 ≈ matches the cloud dot, and a
  // touch above that (0.6) makes the session-colour pick out without
  // crowding the surrounding cluster.
  if (!hasLine && geometry.anchor) {
    return (
      <mesh position={geometry.anchor}>
        <sphereGeometry args={[POINT_SIZE * 0.6, 10, 10]} />
        <meshBasicMaterial color={geometry.color} />
      </mesh>
    );
  }

  return (
    <>
      {hasLine && (
        <ThickLineSegments
          positions={geometry.lineVerts}
          color={geometry.color}
          lineWidth={4}
        />
      )}
      {geometry.anchor && (
        // Hollow wireframe sphere — mirrors the 2-D view's anchor ring.
        // Sized just slightly above the underlying point so it reads as
        // "this dot has a session" without dominating the scene.
        <mesh position={geometry.anchor}>
          <sphereGeometry args={[POINT_SIZE * 1.2, 10, 10]} />
          <meshBasicMaterial
            color={geometry.color}
            wireframe
            transparent
            opacity={0.9}
          />
        </mesh>
      )}
      {geometry.terminal && (
        <mesh position={geometry.terminal}>
          <sphereGeometry args={[POINT_SIZE * 0.9, 10, 10]} />
          <meshBasicMaterial color={geometry.color} />
        </mesh>
      )}
    </>
  );
}

/// Axis lines from -1 to +1 on each axis. Helps the eye keep its
/// orientation as the cloud rotates. Subtle so they don't fight with
/// the points.
function SceneAxes() {
  const verts = useMemo(() => {
    const arr = new Float32Array([
      -1, 0, 0, 1, 0, 0, // x
      0, -1, 0, 0, 1, 0, // y
      0, 0, -1, 0, 0, 1, // z
    ]);
    return arr;
  }, []);
  return (
    <lineSegments>
      <bufferGeometry>
        <bufferAttribute
          attach="attributes-position"
          args={[verts, 3]}
        />
      </bufferGeometry>
      <lineBasicMaterial color="#444" transparent opacity={0.35} />
    </lineSegments>
  );
}

/// Attach OrbitControls to the active camera + canvas. Done imperatively
/// so we don't need @react-three/drei. `enableDamping` smooths the
/// inertia after the user lets go of a drag — feels much better than
/// the default jerk-to-stop.
function Controls() {
  const { camera, gl } = useThree();
  useEffect(() => {
    const controls = new OrbitControls(camera, gl.domElement);
    controls.enableDamping = true;
    controls.dampingFactor = 0.08;
    // Pan with right-drag or two-finger drag, not the default screen
    // pan (which would feel un-3-D). Keeps left-drag for rotate.
    controls.screenSpacePanning = true;
    return () => controls.dispose();
  }, [camera, gl]);
  return null;
}

/// Crank up the raycaster's points threshold so hover hits are
/// forgiving. Three's default (0.1 in screen space) is way too coarse
/// for our unit-cube cloud where points are < 0.05 apart.
function RaycasterTuning() {
  const { raycaster } = useThree();
  useEffect(() => {
    raycaster.params.Points = { threshold: RAY_THRESHOLD };
  }, [raycaster]);
  return null;
}

function HoverTooltip({
  point,
  cursor,
  loading,
  neighbours,
  neighboursLoading,
  wrapRef,
}: {
  point: LatentSpacePoint;
  cursor: { x: number; y: number };
  loading: boolean;
  neighbours:
    | ReadonlyArray<{ track_id: string; cosine_distance: number }>
    | null;
  neighboursLoading: boolean;
  wrapRef: React.RefObject<HTMLDivElement | null>;
}) {
  // Position relative to the wrapper, like the 2-D tooltip does. The
  // wrapper is the only stable ancestor we can use for absolute
  // positioning (the page scrolls; document coords would drift).
  const rect = wrapRef.current?.getBoundingClientRect();
  if (!rect) return null;
  const localX = cursor.x - rect.left;
  const localY = cursor.y - rect.top;
  const flipX = localX > rect.width / 2;
  const flipY = localY > rect.height - 80;
  const style: React.CSSProperties = {
    position: "absolute",
    left: flipX ? undefined : localX + 12,
    right: flipX ? rect.width - localX + 12 : undefined,
    top: flipY ? undefined : localY + 12,
    bottom: flipY ? rect.height - localY + 12 : undefined,
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
          {neighbours.slice(0, 5).map((n) => (
            <div
              key={n.track_id}
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
                <code>{n.track_id.slice(0, 8)}</code>
              </span>
              <span style={{ color: "var(--muted)", fontVariantNumeric: "tabular-nums" }}>
                {n.cosine_distance.toFixed(3)}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

/// Convert "#rrggbb" → THREE-style linear RGB (each channel in [0,1]).
/// Bypasses three.js's `Color` constructor to avoid creating a new
/// object per point — we're writing into a Float32Array that lives for
/// the dataset's lifetime.
function hexToLinearRgb(hex: string): { r: number; g: number; b: number } {
  const r = parseInt(hex.slice(1, 3), 16) / 255;
  const g = parseInt(hex.slice(3, 5), 16) / 255;
  const b = parseInt(hex.slice(5, 7), 16) / 255;
  return { r, g, b };
}
