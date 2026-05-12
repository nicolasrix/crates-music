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
