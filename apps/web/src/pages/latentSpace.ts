// Pure geometry helpers for the latent-space scatter. Imported by both
// the React page and its vitest unit tests; kept DOM-free so the tests
// run in the default vitest environment (no happy-dom dependency).
//
// Coordinate conventions:
//   * Data space — whatever UMAP emitted; x/y are unbounded floats.
//   * Canvas space — pixels, top-left origin (y grows downwards).
// `scaleToCanvas` is the single bridge between the two. Inverting it
// for hover-picking is brute force at our scale (≤10⁴ points).

export interface ScatterPoint {
  track_id: string;
  x: number;
  y: number;
}

export interface DataBounds {
  minX: number;
  maxX: number;
  minY: number;
  maxY: number;
}

export interface CanvasGeometry {
  width: number;
  height: number;
  /** Inset on every side so points at the bounds-edge aren't clipped. */
  padding: number;
}

export function computeBounds(points: readonly ScatterPoint[]): DataBounds | null {
  if (points.length === 0) return null;
  let minX = Infinity;
  let maxX = -Infinity;
  let minY = Infinity;
  let maxY = -Infinity;
  for (const p of points) {
    if (p.x < minX) minX = p.x;
    if (p.x > maxX) maxX = p.x;
    if (p.y < minY) minY = p.y;
    if (p.y > maxY) maxY = p.y;
  }
  return { minX, maxX, minY, maxY };
}

export interface PixelPoint {
  px: number;
  py: number;
}

export function scaleToCanvas(
  point: { x: number; y: number },
  bounds: DataBounds,
  canvas: CanvasGeometry,
): PixelPoint {
  const innerW = canvas.width - 2 * canvas.padding;
  const innerH = canvas.height - 2 * canvas.padding;
  const rangeX = bounds.maxX - bounds.minX;
  const rangeY = bounds.maxY - bounds.minY;
  // Degenerate ranges collapse to centre — avoids NaN from /0 and keeps
  // the cluster visible (otherwise points pile up at (padding, padding)).
  const nx = rangeX === 0 ? 0.5 : (point.x - bounds.minX) / rangeX;
  const ny = rangeY === 0 ? 0.5 : (point.y - bounds.minY) / rangeY;
  return {
    px: canvas.padding + nx * innerW,
    // Invert y so data's +y is "up" on screen, matching how scatter
    // plots are traditionally read.
    py: canvas.padding + (1 - ny) * innerH,
  };
}

// --- 3-D bounds + cube normalisation -------------------------------------
//
// Used by LatentSpace3D. The R3F default camera frames a roughly unit
// cube nicely, so we map the data extents into [-1, 1]^3. Keeping these
// helpers pure (no three.js types) means the unit tests run in vitest's
// default environment alongside the rest of latentSpace.ts.

export interface ScatterPoint3D {
  track_id: string;
  x: number;
  y: number;
  z: number;
}

export interface DataBounds3D {
  minX: number;
  maxX: number;
  minY: number;
  maxY: number;
  minZ: number;
  maxZ: number;
}

export function compute3dBounds(points: readonly ScatterPoint3D[]): DataBounds3D | null {
  if (points.length === 0) return null;
  let minX = Infinity;
  let maxX = -Infinity;
  let minY = Infinity;
  let maxY = -Infinity;
  let minZ = Infinity;
  let maxZ = -Infinity;
  for (const p of points) {
    if (p.x < minX) minX = p.x;
    if (p.x > maxX) maxX = p.x;
    if (p.y < minY) minY = p.y;
    if (p.y > maxY) maxY = p.y;
    if (p.z < minZ) minZ = p.z;
    if (p.z > maxZ) maxZ = p.z;
  }
  return { minX, maxX, minY, maxY, minZ, maxZ };
}

/// Linear remap from data extents to the [-1, 1] unit cube. A degenerate
/// axis (min == max) collapses to 0 — matches `scaleToCanvas`'s
/// centre-of-canvas behaviour for the 2-D case and avoids NaN positions
/// that would crash the buffer-geometry upload.
export function normalizeTo3dCube(
  point: { x: number; y: number; z: number },
  bounds: DataBounds3D,
): { x: number; y: number; z: number } {
  const rangeX = bounds.maxX - bounds.minX;
  const rangeY = bounds.maxY - bounds.minY;
  const rangeZ = bounds.maxZ - bounds.minZ;
  const nx = rangeX === 0 ? 0 : ((point.x - bounds.minX) / rangeX) * 2 - 1;
  const ny = rangeY === 0 ? 0 : ((point.y - bounds.minY) / rangeY) * 2 - 1;
  const nz = rangeZ === 0 ? 0 : ((point.z - bounds.minZ) / rangeZ) * 2 - 1;
  return { x: nx, y: ny, z: nz };
}

// --- bucketByGenre --------------------------------------------------------
//
// Splits the scatter into legend-sized buckets so each can be drawn in
// its own colour. Two reserved labels — Other and Unknown — collect the
// long tail and the null-genre points. Both are pinned to the end of the
// legend so the eye lands on named groups first.
//
// Palette is Tableau10 (colourblind-friendly, distinguishable against the
// dark canvas background). Other/Unknown use desaturated greys so they
// recede visually without disappearing.

/// Catch-all bucket for tracks whose genre tag falls outside the top-N.
export const OTHER_LABEL = "Other";
/// Bucket for tracks with no `genre` in the metadata cache (Subsonic
/// returned null, or the metadata row is missing entirely).
export const UNKNOWN_LABEL = "Unknown";

const PALETTE = [
  "#4e79a7", // blue
  "#f28e2b", // orange
  "#e15759", // red
  "#76b7b2", // teal
  "#59a14f", // green
  "#edc948", // yellow
  "#b07aa1", // purple
  "#ff9da7", // pink
  "#9c755f", // brown
  "#bab0ac", // gray (lightest of the set)
];
const OTHER_COLOR = "#666b73";
const UNKNOWN_COLOR = "#3a3f4c";

export interface GenreBucket {
  label: string;
  count: number;
  color: string;
}

export interface GenreBucketingResult {
  buckets: GenreBucket[];
  pointsByLabel: Map<string, ScatterPoint[]>;
}

type PointWithGenre = ScatterPoint & { genre: string | null };

export function bucketByGenre(
  points: readonly PointWithGenre[],
  topN: number,
): GenreBucketingResult {
  if (points.length === 0) {
    return { buckets: [], pointsByLabel: new Map() };
  }

  // First pass: split null vs named, tally counts, and preserve input
  // order per genre. A single Map keyed by genre name keeps both the
  // count and the per-genre slice in one place.
  const namedGroups = new Map<string, ScatterPoint[]>();
  const unknownGroup: ScatterPoint[] = [];
  for (const p of points) {
    const slim: ScatterPoint = { track_id: p.track_id, x: p.x, y: p.y };
    if (p.genre === null) {
      unknownGroup.push(slim);
    } else {
      const existing = namedGroups.get(p.genre);
      if (existing) {
        existing.push(slim);
      } else {
        namedGroups.set(p.genre, [slim]);
      }
    }
  }

  // Rank named groups: count desc, ties broken by label asc. Stable
  // ordering is critical — the palette index follows order, so an
  // unstable sort would shuffle colours between renders.
  const ranked: Array<[string, ScatterPoint[]]> = Array.from(namedGroups);
  ranked.sort((a, b) => {
    const dc = b[1].length - a[1].length;
    return dc !== 0 ? dc : a[0].localeCompare(b[0]);
  });

  const top = ranked.slice(0, topN);
  const tail = ranked.slice(topN);

  const buckets: GenreBucket[] = [];
  const pointsByLabel = new Map<string, ScatterPoint[]>();

  top.forEach(([label, slice], i) => {
    // Modulo guarantees a value; the `?? OTHER_COLOR` is for TS's
    // noUncheckedIndexedAccess — runtime can never hit the fallback at
    // current topN.
    const color = PALETTE[i % PALETTE.length] ?? OTHER_COLOR;
    buckets.push({ label, count: slice.length, color });
    pointsByLabel.set(label, slice);
  });

  if (tail.length > 0) {
    const merged = tail.flatMap(([, slice]) => slice);
    buckets.push({
      label: OTHER_LABEL,
      count: merged.length,
      color: OTHER_COLOR,
    });
    pointsByLabel.set(OTHER_LABEL, merged);
  }

  if (unknownGroup.length > 0) {
    buckets.push({
      label: UNKNOWN_LABEL,
      count: unknownGroup.length,
      color: UNKNOWN_COLOR,
    });
    pointsByLabel.set(UNKNOWN_LABEL, unknownGroup);
  }

  return { buckets, pointsByLabel };
}

// --- continuous-value colour encoding -----------------------------------
//
// When the user picks "colour by → PCn" we map the per-point PCA value
// to a colour along a perceptually-uniform gradient. Doing this in a
// pure helper keeps the canvas effect testable and lets us swap
// palettes without touching the render code.

/// Min/max envelope of a numeric channel across the input rows.
/// Returns `null` when no rows have a finite value — the caller
/// should render those points in a neutral colour (the channel
/// carries no usable signal).
export function rangeOfFiniteValues(
  values: ReadonlyArray<number | null>,
): { min: number; max: number } | null {
  let min = Infinity;
  let max = -Infinity;
  let any = false;
  for (const v of values) {
    if (v === null || !Number.isFinite(v)) continue;
    if (v < min) min = v;
    if (v > max) max = v;
    any = true;
  }
  return any ? { min, max } : null;
}

/// Linearly remap `value` into `[0, 1]` using `min..max`. Out-of-range
/// values are clamped (not clipped — they should still draw, just at
/// the palette extremes). When the range degenerates (`min == max`),
/// returns `0.5` so every point gets the palette's mid colour rather
/// than an arbitrary endpoint.
export function normalizeInto01(
  value: number,
  range: { min: number; max: number },
): number {
  if (range.max === range.min) return 0.5;
  const t = (value - range.min) / (range.max - range.min);
  if (t < 0) return 0;
  if (t > 1) return 1;
  return t;
}

/// Sample a viridis-ish gradient at `t ∈ [0, 1]`. We embed a five-stop
/// approximation rather than pulling in d3-scale-chromatic: the palette
/// stays self-contained and we don't pay a dependency for 60 lines of
/// piecewise linear interpolation.
///
/// Stops sampled from matplotlib's viridis at 0 / 0.25 / 0.5 / 0.75 / 1.
/// Perceptually-uniform end-to-end; safe for colourblind users (one of
/// the reasons viridis exists).
const VIRIDIS_STOPS: ReadonlyArray<[number, number, number]> = [
  [68, 1, 84], // 0.00 — deep violet
  [59, 82, 139], // 0.25 — indigo
  [33, 145, 140], // 0.50 — teal
  [94, 201, 98], // 0.75 — green
  [253, 231, 37], // 1.00 — yellow
];

export function viridis(t: number): string {
  // Clamp instead of throwing: the helper is called from a hot render
  // loop, and a stray NaN shouldn't crash the canvas.
  const x = Number.isFinite(t) ? Math.max(0, Math.min(1, t)) : 0;
  // Each segment covers 1/(N-1) of the range; figure out which.
  const seg = x * (VIRIDIS_STOPS.length - 1);
  const lo = Math.min(VIRIDIS_STOPS.length - 1, Math.floor(seg));
  const hi = Math.min(VIRIDIS_STOPS.length - 1, lo + 1);
  const f = seg - lo;
  const a = VIRIDIS_STOPS[lo]!;
  const b = VIRIDIS_STOPS[hi]!;
  const r = Math.round(a[0] + (b[0] - a[0]) * f);
  const g = Math.round(a[1] + (b[1] - a[1]) * f);
  const bl = Math.round(a[2] + (b[2] - a[2]) * f);
  return `#${r.toString(16).padStart(2, "0")}${g
    .toString(16)
    .padStart(2, "0")}${bl.toString(16).padStart(2, "0")}`;
}

// --- session path encoding -----------------------------------------------
//
// A session is rendered on top of the scatter as a polyline through the
// projected points of its events. Each segment carries cosine_distance
// in the original 512-D CLAP space — UMAP doesn't preserve that
// globally, so we encode it on the line itself. Closer in latent space
// → thicker stroke ("natural neighbour"); further → thinner ("a leap").

export interface PathWidthRange {
  /** Stroke width (px) at `cosine_distance = 0` — colinear in CLAP space. */
  maxWidth: number;
  /** Stroke width (px) at `cosine_distance ≥ cap`. */
  minWidth: number;
  /** Distance value at which the line collapses to `minWidth`. CLAP
   *  cosine distances cluster in `[0, 1]` in practice, so 1.0 is a
   *  sensible default cap. */
  cap: number;
}

/// Map a cosine distance to a stroke width. `null` (missing embedding)
/// returns `maxWidth` so the segment still draws — the dashed style
/// rendered alongside conveys the "unknown" state, not the width.
export function cosineDistanceToWidth(
  distance: number | null,
  range: PathWidthRange,
): number {
  if (distance === null) return range.maxWidth;
  if (distance <= 0) return range.maxWidth;
  if (distance >= range.cap) return range.minWidth;
  const t = distance / range.cap;
  return range.maxWidth + (range.minWidth - range.maxWidth) * t;
}

/// Stable hue for a session id. Hash → degree on the colour wheel; we
/// keep saturation/lightness fixed for legibility on the dark canvas.
export function sessionHue(sessionId: string): number {
  let h = 0;
  for (let i = 0; i < sessionId.length; i++) {
    h = (h * 31 + sessionId.charCodeAt(i)) >>> 0;
  }
  return h % 360;
}

/// Project an ordered list of track ids onto the scatter points,
/// dropping ids the projection doesn't contain. Order of the input ids
/// is preserved in the output — important because the hover-neighbour
/// overlay receives ids pre-sorted by cosine distance and draws them
/// in that order (closest first, so further rings render on top of
/// fainter ones).
///
/// Allocation note: builds a single Map keyed by `track_id` for O(1)
/// lookup, so the overall cost is O(points + ids) regardless of how
/// many neighbours the backend returned.
export function pickPointsByIds<P extends { track_id: string }>(
  ids: readonly string[],
  points: readonly P[],
): P[] {
  if (ids.length === 0) return [];
  const byId = new Map<string, P>();
  for (const p of points) byId.set(p.track_id, p);
  const out: P[] = [];
  for (const id of ids) {
    const hit = byId.get(id);
    if (hit) out.push(hit);
  }
  return out;
}

/// Continuous-channel modes the "colour by" dropdown can map to a
/// viridis gradient. Excludes "genre" (which uses the bucketed palette
/// pathway) so callers can statically distinguish the two regimes.
export type ContinuousColorMode = "pc1" | "pc2" | "pc3" | "pc4";

/// Subset of `LatentSpacePoint` the colour-channel readers care about.
/// Keeps the helper independent of the API module so it stays in the
/// same "pure helpers" file as the rest of latentSpace.ts.
export interface ContinuousChannelPoint {
  pc1: number | null;
  pc2: number | null;
  pc3: number | null;
  pc4: number | null;
}

/// Read the value of `mode` from `point`. `null` when the projection
/// didn't populate that channel — caller should paint the dot in the
/// neutral colour rather than picking a misleading gradient stop.
export function colorChannelValue(
  point: ContinuousChannelPoint,
  mode: ContinuousColorMode,
): number | null {
  switch (mode) {
    case "pc1":
      return point.pc1;
    case "pc2":
      return point.pc2;
    case "pc3":
      return point.pc3;
    case "pc4":
      return point.pc4;
  }
}

/// Invert `bucketByGenre`'s output into a track_id → bucket-colour
/// lookup. Used by the 3-D scene where colour has to be resolved per
/// point (one Float32Array slot per dot, all uploaded as a single
/// buffer) rather than per bucket as the 2-D canvas does.
///
/// Returns an empty Map for the empty bucketing — callers should fall
/// back to a neutral grey for unmapped tracks.
export function genreColorByTrackId(
  bucketing: GenreBucketingResult,
): Map<string, string> {
  const out = new Map<string, string>();
  for (const bucket of bucketing.buckets) {
    const slice = bucketing.pointsByLabel.get(bucket.label);
    if (!slice) continue;
    for (const p of slice) out.set(p.track_id, bucket.color);
  }
  return out;
}

/// Minimal track metadata the session-tracks panel needs. Sourced from
/// the latent-space points (which already carry these fields) so the
/// panel doesn't need to issue per-track `getSong` calls just to label
/// rows. Album is included for the (eventual) tooltip / secondary line.
export interface TrackMetadata {
  title: string | null;
  artist: string | null;
  album: string | null;
}

/// One row in the session-tracks panel. `prev_distance` is the cosine
/// distance to the immediately preceding event in the same session.
/// Two flavours of null:
///   * Row 0 — there is no predecessor.
///   * Row i>0 — the segment's distance was null (one of the tracks
///     lacks a `done` embedding under the active model).
/// The panel renders both the same way ("—"), but keeping them distinct
/// in the data model means callers can surface "embedding missing" later
/// without re-deriving the chain.
export interface SessionTrackRow {
  track_id: string;
  title: string | null;
  artist: string | null;
  album: string | null;
  prev_distance: number | null;
}

/// Build the row data for a session's track panel.
///
/// `events` and `segments` come straight from the `SessionItem`
/// returned by `fetchRecommendSessions({ includeEvents: true })`; the
/// metadata map is keyed by `track_id` and typically built once per
/// (sessions, points) change in the caller.
export function buildSessionTrackRows(
  events: readonly { track_id: string }[],
  segments: readonly { cosine_distance: number | null }[] | undefined,
  byTrack: ReadonlyMap<string, TrackMetadata>,
): SessionTrackRow[] {
  const rows: SessionTrackRow[] = [];
  for (let i = 0; i < events.length; i++) {
    const ev = events[i]!;
    const meta = byTrack.get(ev.track_id);
    // i === 0 has no left segment; later rows look one to the left.
    const seg = i === 0 ? null : (segments?.[i - 1] ?? null);
    rows.push({
      track_id: ev.track_id,
      title: meta?.title ?? null,
      artist: meta?.artist ?? null,
      album: meta?.album ?? null,
      prev_distance: seg ? seg.cosine_distance : null,
    });
  }
  return rows;
}

/// Nearest scatter point to the cursor in *pixel space*, or null if no
/// point lies within `radiusPx`. Linear scan — adequate at our N.
export function pickNearestPoint(
  cursor: PixelPoint,
  points: readonly ScatterPoint[],
  bounds: DataBounds,
  canvas: CanvasGeometry,
  radiusPx: number,
): ScatterPoint | null {
  const radiusSq = radiusPx * radiusPx;
  let best: ScatterPoint | null = null;
  let bestDistSq = Infinity;
  for (const p of points) {
    const { px, py } = scaleToCanvas(p, bounds, canvas);
    const dx = px - cursor.px;
    const dy = py - cursor.py;
    const distSq = dx * dx + dy * dy;
    // `<` (not `<=`) so the first match in input order wins on exact ties.
    if (distSq <= radiusSq && distSq < bestDistSq) {
      best = p;
      bestDistSq = distSq;
    }
  }
  return best;
}
