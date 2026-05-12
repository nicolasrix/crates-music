// Pure helpers for the latent-space canvas scatter. These do all the
// geometry the React component depends on: figuring out the data bounds,
// mapping data-space points to pixel-space, and inverting the hover
// position back to the nearest point.
//
// Kept side-effect-free so a vitest run doesn't need a DOM canvas.

import { describe, expect, it } from "vitest";
import {
  bucketByGenre,
  computeBounds,
  cosineDistanceToWidth,
  OTHER_LABEL,
  UNKNOWN_LABEL,
  pickNearestPoint,
  scaleToCanvas,
  sessionHue,
  type ScatterPoint,
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
