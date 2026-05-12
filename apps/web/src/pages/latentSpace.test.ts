// Pure helpers for the latent-space canvas scatter. These do all the
// geometry the React component depends on: figuring out the data bounds,
// mapping data-space points to pixel-space, and inverting the hover
// position back to the nearest point.
//
// Kept side-effect-free so a vitest run doesn't need a DOM canvas.

import { describe, expect, it } from "vitest";
import {
  bucketByGenre,
  colorChannelValue,
  compute3dBounds,
  computeBounds,
  cosineDistanceToWidth,
  normalizeInto01,
  normalizeTo3dCube,
  OTHER_LABEL,
  pickPointsByIds,
  UNKNOWN_LABEL,
  pickNearestPoint,
  rangeOfFiniteValues,
  scaleToCanvas,
  sessionHue,
  viridis,
  type ContinuousChannelPoint,
  type ScatterPoint,
  type ScatterPoint3D,
} from "./latentSpace";

const pt = (track_id: string, x: number, y: number): ScatterPoint => ({
  track_id,
  x,
  y,
});

describe("computeBounds", () => {
  it("returns null for an empty input", () => {
    // Caller renders the empty-state message; no axes are drawn.
    expect(computeBounds([])).toBeNull();
  });

  it("returns the min/max envelope across the input", () => {
    const b = computeBounds([pt("a", 1, 10), pt("b", -2, 4), pt("c", 7, -3)]);
    expect(b).toEqual({ minX: -2, maxX: 7, minY: -3, maxY: 10 });
  });

  it("collapses to a degenerate range when all points share a coordinate", () => {
    // A real dataset can have constant x (or y) after a bad UMAP run. The
    // scaler handles that by pinning to the canvas centre, but the bounds
    // must still report the actual range — we don't fudge it.
    const b = computeBounds([pt("a", 5, 5), pt("b", 5, 5)]);
    expect(b).toEqual({ minX: 5, maxX: 5, minY: 5, maxY: 5 });
  });
});

describe("scaleToCanvas", () => {
  const bounds = { minX: 0, maxX: 10, minY: 0, maxY: 10 };
  const canvas = { width: 100, height: 200, padding: 10 };

  it("maps the bounds-min corner to the bottom-left of the inner box", () => {
    // y axis is inverted in canvas space — minY (data) → maxY (pixel).
    expect(scaleToCanvas({ x: 0, y: 0 }, bounds, canvas)).toEqual({
      px: 10,
      py: 190,
    });
  });

  it("maps the bounds-max corner to the top-right of the inner box", () => {
    expect(scaleToCanvas({ x: 10, y: 10 }, bounds, canvas)).toEqual({
      px: 90,
      py: 10,
    });
  });

  it("scales mid-range points proportionally", () => {
    // x=5 lands halfway → px = padding + 0.5 * (width - 2*padding) = 50
    // y=5 lands halfway with inverted axis → py = 100
    expect(scaleToCanvas({ x: 5, y: 5 }, bounds, canvas)).toEqual({
      px: 50,
      py: 100,
    });
  });

  it("pins to the inner-box centre when the data range is degenerate", () => {
    // Real UMAP output can collapse a dimension. Dividing by 0 would give
    // NaN — instead we centre the points so the user sees them rather
    // than a blank canvas.
    const flat = { minX: 5, maxX: 5, minY: 5, maxY: 5 };
    expect(scaleToCanvas({ x: 5, y: 5 }, flat, canvas)).toEqual({
      px: 50,
      py: 100,
    });
  });
});

describe("pickNearestPoint", () => {
  // Simple corner-layout for hover assertions.
  const bounds = { minX: 0, maxX: 10, minY: 0, maxY: 10 };
  const canvas = { width: 100, height: 100, padding: 10 };
  const points: ScatterPoint[] = [
    pt("a", 0, 0), //  → (10, 90)
    pt("b", 10, 10), // → (90, 10)
    pt("c", 5, 5), //  → (50, 50)
  ];

  it("returns null when the cursor is beyond the radius from every point", () => {
    expect(pickNearestPoint({ px: 0, py: 0 }, points, bounds, canvas, 5)).toBeNull();
  });

  it("returns the nearest point in pixel space", () => {
    // Cursor near (10, 90) — exactly point "a".
    const got = pickNearestPoint({ px: 11, py: 89 }, points, bounds, canvas, 5);
    expect(got?.track_id).toBe("a");
  });

  it("breaks ties by the first match in input order", () => {
    // Two co-located points: nearest is "a", as it comes first.
    const dupes = [pt("a", 0, 0), pt("a-dup", 0, 0)];
    const got = pickNearestPoint({ px: 10, py: 90 }, dupes, bounds, canvas, 5);
    expect(got?.track_id).toBe("a");
  });

  it("respects an explicit hover radius", () => {
    // 4 pixels away from point "c" (50, 50). Default radius 5 → match.
    // Tightening to 3 → no match.
    const cursor = { px: 53, py: 53 }; // √(9+9) ≈ 4.24
    expect(pickNearestPoint(cursor, points, bounds, canvas, 5)?.track_id).toBe("c");
    expect(pickNearestPoint(cursor, points, bounds, canvas, 3)).toBeNull();
  });
});

// --- bucketByGenre --------------------------------------------------------

// Adds a genre to the ScatterPoint shape so we can drive the bucketing.
// The helper itself is what we want to test, not the shape conversions.
const gpt = (
  track_id: string,
  genre: string | null,
): ScatterPoint & { genre: string | null } => ({
  track_id,
  x: 0,
  y: 0,
  genre,
});

describe("bucketByGenre", () => {
  it("returns empty buckets for empty input", () => {
    // No data → legend renders nothing → caller falls back to the empty
    // hint. We don't synthesise an Unknown bucket out of thin air.
    const got = bucketByGenre([], 10);
    expect(got.buckets).toEqual([]);
    expect(got.pointsByLabel.size).toBe(0);
  });

  it("groups every null-genre point under the Unknown bucket", () => {
    // The simplest projection — no Subsonic genre tags anywhere. One
    // catch-all bucket is correct; we don't want to dilute Unknown with
    // a fake 'no-genre' bucket that the user has to mentally rename.
    const got = bucketByGenre(
      [gpt("a", null), gpt("b", null), gpt("c", null)],
      10,
    );
    expect(got.buckets.length).toBe(1);
    expect(got.buckets[0]?.label).toBe(UNKNOWN_LABEL);
    expect(got.buckets[0]?.count).toBe(3);
    expect(got.pointsByLabel.get(UNKNOWN_LABEL)?.length).toBe(3);
  });

  it("orders buckets by count desc, ties broken alphabetically", () => {
    // Determinism in the legend matters more than it sounds — the
    // palette index follows order, so an unstable sort would shuffle
    // colours on every re-render. Alphabetical tiebreak gives operators
    // a stable visual.
    const got = bucketByGenre(
      [
        gpt("1", "Rock"),
        gpt("2", "Rock"),
        gpt("3", "Jazz"),
        gpt("4", "Pop"),
        gpt("5", "Pop"),
      ],
      10,
    );
    // Rock=2, Pop=2 (tie → Pop before Rock alphabetically), Jazz=1.
    expect(got.buckets.map((b) => b.label)).toEqual(["Pop", "Rock", "Jazz"]);
  });

  it("collapses the long tail into Other when more than topN named genres exist", () => {
    // 4 named genres, topN=2 → top 2 by count, plus Other holding the
    // rest. This is the realistic shape — Subsonic libraries often have
    // a tail of 50+ rare genre tags that would otherwise blow out the
    // legend.
    const got = bucketByGenre(
      [
        gpt("1", "Rock"),
        gpt("2", "Rock"),
        gpt("3", "Rock"),
        gpt("4", "Jazz"),
        gpt("5", "Jazz"),
        gpt("6", "Ambient"),
        gpt("7", "Country"),
      ],
      2,
    );
    expect(got.buckets.map((b) => b.label)).toEqual([
      "Rock",
      "Jazz",
      OTHER_LABEL,
    ]);
    expect(got.buckets.find((b) => b.label === OTHER_LABEL)?.count).toBe(2);
    expect(got.pointsByLabel.get(OTHER_LABEL)?.length).toBe(2);
  });

  it("places Other and Unknown after the named buckets regardless of count", () => {
    // Critical for legend readability: even if a 'no-genre' bucket has
    // the highest count, the catch-alls visually subordinate to named
    // groups so the eye lands on the meaningful categories first.
    const got = bucketByGenre(
      [
        gpt("a", null),
        gpt("b", null),
        gpt("c", null),
        gpt("d", null),
        gpt("e", "Rock"),
        gpt("f", "Jazz"),
        gpt("g", "Pop"),
        gpt("h", "Country"),
      ],
      2,
    );
    const labels = got.buckets.map((b) => b.label);
    // Named groups (Rock=1, Pop=1 by tiebreak before all = 1), then Other, then Unknown.
    // The exact named order is alphabetical at count=1 → Country, Jazz, Pop, Rock; top 2 = Country, Jazz.
    expect(labels.slice(0, 2)).toEqual(["Country", "Jazz"]);
    expect(labels[labels.length - 2]).toBe(OTHER_LABEL);
    expect(labels[labels.length - 1]).toBe(UNKNOWN_LABEL);
  });

  it("assigns a distinct color per bucket", () => {
    // Each legend row needs its own colour; the palette is finite but
    // big enough that no two buckets share a colour at realistic topN.
    const got = bucketByGenre(
      [
        gpt("1", "Rock"),
        gpt("2", "Jazz"),
        gpt("3", "Pop"),
        gpt("4", null),
      ],
      5,
    );
    const colors = got.buckets.map((b) => b.color);
    const unique = new Set(colors);
    expect(unique.size).toBe(colors.length);
    // Every bucket must have a non-empty colour string.
    expect(colors.every((c) => c.length > 0)).toBe(true);
  });

  it("preserves input order within a bucket", () => {
    // The drawing loop is order-dependent: later points draw on top of
    // earlier ones. Preserving input order keeps later additions to the
    // gateway response visually consistent with their array index.
    const got = bucketByGenre(
      [
        gpt("first", "Rock"),
        gpt("second", "Rock"),
        gpt("third", "Rock"),
      ],
      10,
    );
    const rock = got.pointsByLabel.get("Rock")!;
    expect(rock.map((p) => p.track_id)).toEqual(["first", "second", "third"]);
  });
});

describe("cosineDistanceToWidth", () => {
  const range = { minWidth: 0.5, maxWidth: 5, cap: 1 };

  it("returns maxWidth for distance = 0 (colinear in latent space)", () => {
    expect(cosineDistanceToWidth(0, range)).toBe(5);
  });

  it("returns minWidth at or above the cap", () => {
    expect(cosineDistanceToWidth(1, range)).toBe(0.5);
    expect(cosineDistanceToWidth(2, range)).toBe(0.5);
  });

  it("returns maxWidth for null (unknown distance)", () => {
    // Stroke still draws — the dashed style elsewhere conveys 'unknown',
    // not the width. We don't want to silently collapse the segment to a
    // hairline just because one embedding hadn't been ingested yet.
    expect(cosineDistanceToWidth(null, range)).toBe(5);
  });

  it("interpolates linearly between max and min across the cap range", () => {
    // Half the cap distance → halfway between widths.
    expect(cosineDistanceToWidth(0.5, range)).toBeCloseTo((5 + 0.5) / 2, 6);
  });

  it("clamps negative distances to maxWidth (defensive)", () => {
    // Cosine distance is mathematically ≥ 0, but floats from JSON can
    // come in as -0 or tiny negatives — don't let those produce widths
    // above maxWidth.
    expect(cosineDistanceToWidth(-0.01, range)).toBe(5);
  });
});

describe("sessionHue", () => {
  it("is deterministic for a given id", () => {
    expect(sessionHue("abc")).toBe(sessionHue("abc"));
  });

  it("returns a value in [0, 360)", () => {
    for (const id of ["", "x", "long-session-id-1234", "s1", "s2"]) {
      const h = sessionHue(id);
      expect(h).toBeGreaterThanOrEqual(0);
      expect(h).toBeLessThan(360);
    }
  });

  it("usually differs across distinct ids", () => {
    // Not a hard guarantee (any hash has collisions), but small ids
    // shouldn't collide trivially. This catches a regression where the
    // hash was clamped to <8.
    const hues = new Set(["s1", "s2", "s3", "s4", "s5"].map(sessionHue));
    expect(hues.size).toBeGreaterThanOrEqual(4);
  });
});

describe("rangeOfFiniteValues", () => {
  it("returns min/max across finite numeric values", () => {
    expect(rangeOfFiniteValues([1, 3, -2, 5])).toEqual({ min: -2, max: 5 });
  });

  it("skips null entries", () => {
    expect(rangeOfFiniteValues([null, 1, null, 4])).toEqual({ min: 1, max: 4 });
  });

  it("returns null when every entry is null", () => {
    // Caller renders these points in a neutral colour — the channel
    // has no signal.
    expect(rangeOfFiniteValues([null, null])).toBeNull();
    expect(rangeOfFiniteValues([])).toBeNull();
  });

  it("skips non-finite values like NaN and Infinity", () => {
    expect(rangeOfFiniteValues([NaN, 1, Infinity, 2, -Infinity])).toEqual({
      min: 1,
      max: 2,
    });
  });
});

describe("normalizeInto01", () => {
  const r = { min: 0, max: 10 };

  it("maps endpoints to 0 and 1", () => {
    expect(normalizeInto01(0, r)).toBe(0);
    expect(normalizeInto01(10, r)).toBe(1);
  });

  it("interpolates linearly", () => {
    expect(normalizeInto01(2.5, r)).toBeCloseTo(0.25, 6);
  });

  it("clamps out-of-range inputs", () => {
    expect(normalizeInto01(-5, r)).toBe(0);
    expect(normalizeInto01(99, r)).toBe(1);
  });

  it("returns 0.5 for a degenerate range", () => {
    // Every point's PC value being identical is plausible on a tiny
    // dev dataset. Returning 0.5 gives them the palette's mid colour
    // instead of an arbitrary endpoint.
    expect(normalizeInto01(7, { min: 3, max: 3 })).toBe(0.5);
  });
});

describe("viridis", () => {
  it("returns a 7-char hex colour at every stop and between", () => {
    for (const t of [0, 0.25, 0.5, 0.75, 1]) {
      const c = viridis(t);
      expect(c).toMatch(/^#[0-9a-f]{6}$/);
    }
  });

  it("clamps out-of-range values", () => {
    expect(viridis(-1)).toBe(viridis(0));
    expect(viridis(2)).toBe(viridis(1));
  });

  it("interpolates between two stops monotonically in brightness", () => {
    // Viridis is luminance-monotonic — higher t means brighter output.
    // A simple sanity check that interpolation order isn't broken.
    const dark = viridis(0);
    const mid = viridis(0.5);
    const bright = viridis(1);
    const lum = (hex: string) =>
      parseInt(hex.slice(1, 3), 16) +
      parseInt(hex.slice(3, 5), 16) +
      parseInt(hex.slice(5, 7), 16);
    expect(lum(mid)).toBeGreaterThan(lum(dark));
    expect(lum(bright)).toBeGreaterThan(lum(mid));
  });

  it("handles NaN gracefully (renders as the bottom of the palette)", () => {
    expect(viridis(NaN)).toBe(viridis(0));
  });
});

describe("pickPointsByIds", () => {
  // Helper used by the hover-neighbour overlay: given an ordered list
  // of track ids (returned by the backend, ascending in cosine
  // distance), produce the matching scatter points in the same order.
  // Order matters — the canvas iterates this list to draw rings whose
  // visual emphasis corresponds to neighbour rank.

  it("returns an empty array when ids is empty", () => {
    expect(pickPointsByIds([], [pt("a", 0, 0)])).toEqual([]);
  });

  it("preserves the requested id order, not the point input order", () => {
    const points = [pt("a", 0, 0), pt("b", 1, 1), pt("c", 2, 2)];
    const result = pickPointsByIds(["c", "a"], points);
    expect(result.map((p) => p.track_id)).toEqual(["c", "a"]);
  });

  it("silently drops ids that are not present in points", () => {
    // The projection may not include every track the ANN returns —
    // e.g. tracks embedded but not yet projected. Those neighbours
    // simply don't get drawn rather than crashing the overlay.
    const points = [pt("a", 0, 0), pt("c", 2, 2)];
    expect(pickPointsByIds(["a", "missing", "c"], points)).toEqual([
      points[0],
      points[1],
    ]);
  });

  it("is O(n + m) with respect to inputs (no quadratic search)", () => {
    // Build a 1000-point haystack and pick a single needle. If the
    // implementation is quadratic this still passes — but the
    // assertion documents intent.
    const haystack: ScatterPoint[] = Array.from({ length: 1000 }, (_, i) =>
      pt(`t${i}`, i, i),
    );
    const result = pickPointsByIds(["t999"], haystack);
    expect(result).toHaveLength(1);
    expect(result[0]!.track_id).toBe("t999");
  });
});

describe("colorChannelValue", () => {
  // Single source of truth for the "colour by" dropdown: PCs map to
  // their named fields; "umap_z" maps to z. Every continuous mode must
  // return a number-or-null without falling through to undefined —
  // otherwise the canvas would render `undefined` dots as the bottom
  // of the gradient.
  const point: ContinuousChannelPoint = {
    pc1: 0.5,
    pc2: -0.5,
    pc3: 0.25,
    pc4: null,
    z: 1.25,
  };

  it("reads each PC field by mode", () => {
    expect(colorChannelValue(point, "pc1")).toBe(0.5);
    expect(colorChannelValue(point, "pc2")).toBe(-0.5);
    expect(colorChannelValue(point, "pc3")).toBe(0.25);
    expect(colorChannelValue(point, "pc4")).toBeNull();
  });

  it("maps umap_z to the z field", () => {
    // The whole point of the "UMAP z" mode: it's the 3D UMAP's third
    // axis surfaced through colour. The naming must not silently slip
    // back to a PC field.
    expect(colorChannelValue(point, "umap_z")).toBe(1.25);
  });

  it("returns null for a point with no value in the chosen channel", () => {
    const empty: ContinuousChannelPoint = {
      pc1: null,
      pc2: null,
      pc3: null,
      pc4: null,
      z: null,
    };
    expect(colorChannelValue(empty, "umap_z")).toBeNull();
    expect(colorChannelValue(empty, "pc1")).toBeNull();
  });
});

// --- 3-D bounds + cube normalisation -------------------------------------
//
// The R3F scene wants positions in a roughly unit-sized cube so the
// default camera frames the cloud without per-dataset zoom tuning. These
// helpers are the data-space → world-space bridge; everything else in
// LatentSpace3D is pure three.js plumbing.

const pt3 = (track_id: string, x: number, y: number, z: number): ScatterPoint3D => ({
  track_id,
  x,
  y,
  z,
});

describe("compute3dBounds", () => {
  it("returns null for an empty input", () => {
    expect(compute3dBounds([])).toBeNull();
  });

  it("returns min/max across all three axes", () => {
    const points: ScatterPoint3D[] = [
      pt3("a", 1, 2, 3),
      pt3("b", -1, 5, 0),
      pt3("c", 4, -2, 7),
    ];
    expect(compute3dBounds(points)).toEqual({
      minX: -1,
      maxX: 4,
      minY: -2,
      maxY: 5,
      minZ: 0,
      maxZ: 7,
    });
  });

  it("collapses to a single point when the input has one element", () => {
    // Single-point bounds are degenerate on every axis. The downstream
    // normaliser must handle this without dividing by zero — that
    // contract is tested explicitly below.
    expect(compute3dBounds([pt3("a", 3, 4, 5)])).toEqual({
      minX: 3,
      maxX: 3,
      minY: 4,
      maxY: 4,
      minZ: 5,
      maxZ: 5,
    });
  });
});

describe("normalizeTo3dCube", () => {
  const bounds = {
    minX: 0,
    maxX: 10,
    minY: -5,
    maxY: 5,
    minZ: 0,
    maxZ: 2,
  };

  it("maps the minimum of each axis to -1", () => {
    expect(normalizeTo3dCube({ x: 0, y: -5, z: 0 }, bounds)).toEqual({
      x: -1,
      y: -1,
      z: -1,
    });
  });

  it("maps the maximum of each axis to +1", () => {
    expect(normalizeTo3dCube({ x: 10, y: 5, z: 2 }, bounds)).toEqual({
      x: 1,
      y: 1,
      z: 1,
    });
  });

  it("maps the midpoint of each axis to 0", () => {
    expect(normalizeTo3dCube({ x: 5, y: 0, z: 1 }, bounds)).toEqual({
      x: 0,
      y: 0,
      z: 0,
    });
  });

  it("collapses a degenerate axis to 0 rather than producing NaN", () => {
    // When min == max on an axis, the linear map divides by zero. The
    // sentinel value is 0 (the cube centre) — matches scaleToCanvas's
    // 0.5 / centre-of-canvas behaviour for the 2-D case.
    const flat = { minX: 3, maxX: 3, minY: -5, maxY: 5, minZ: 0, maxZ: 2 };
    expect(normalizeTo3dCube({ x: 3, y: 0, z: 1 }, flat)).toEqual({
      x: 0,
      y: 0,
      z: 0,
    });
  });
});
