import { describe, expect, it } from "vitest";
import { fmtBytes, fmtDuration, fmtMs, fmtPlays, fmtRelativePast } from "./format";

describe("fmtPlays", () => {
  it("returns null for missing/zero/negative counts", () => {
    expect(fmtPlays(null)).toBeNull();
    expect(fmtPlays(undefined)).toBeNull();
    expect(fmtPlays(0)).toBeNull();
    expect(fmtPlays(-1)).toBeNull();
    expect(fmtPlays(NaN)).toBeNull();
  });

  it("singular for 1", () => {
    expect(fmtPlays(1)).toBe("1 play");
  });

  it("plural for >1", () => {
    expect(fmtPlays(2)).toBe("2 plays");
    expect(fmtPlays(347)).toBe("347 plays");
  });

  it("floors fractional input rather than rounding", () => {
    expect(fmtPlays(1.9)).toBe("1 play");
  });
});

describe("fmtRelativePast", () => {
  // Frozen "now" for deterministic buckets.
  const NOW = Date.parse("2026-05-10T12:00:00Z");
  const isoMinusMs = (ms: number) => new Date(NOW - ms).toISOString();

  it("returns null for missing or unparseable input", () => {
    expect(fmtRelativePast(null, NOW)).toBeNull();
    expect(fmtRelativePast(undefined, NOW)).toBeNull();
    expect(fmtRelativePast("not-a-date", NOW)).toBeNull();
  });

  it("'just now' under an hour", () => {
    expect(fmtRelativePast(isoMinusMs(30 * 60 * 1000), NOW)).toBe("just now");
  });

  it("'today' between 1 and 24 hours", () => {
    expect(fmtRelativePast(isoMinusMs(2 * 60 * 60 * 1000), NOW)).toBe("today");
    expect(fmtRelativePast(isoMinusMs(23 * 60 * 60 * 1000), NOW)).toBe("today");
  });

  it("'yesterday' between 24 and 48 hours", () => {
    expect(fmtRelativePast(isoMinusMs(36 * 60 * 60 * 1000), NOW)).toBe("yesterday");
  });

  it("days for the rest of the first week", () => {
    expect(fmtRelativePast(isoMinusMs(3 * 24 * 60 * 60 * 1000), NOW)).toBe("3 days ago");
    expect(fmtRelativePast(isoMinusMs(6 * 24 * 60 * 60 * 1000), NOW)).toBe("6 days ago");
  });

  it("'a week ago' rounds the second week", () => {
    expect(fmtRelativePast(isoMinusMs(8 * 24 * 60 * 60 * 1000), NOW)).toBe("a week ago");
  });

  it("weeks bucket up to two months", () => {
    expect(fmtRelativePast(isoMinusMs(21 * 24 * 60 * 60 * 1000), NOW)).toBe("3 weeks ago");
  });

  it("months bucket between 60 days and a year", () => {
    expect(fmtRelativePast(isoMinusMs(90 * 24 * 60 * 60 * 1000), NOW)).toBe("3 months ago");
  });

  it("'a year ago' for the first year past 365 days", () => {
    expect(fmtRelativePast(isoMinusMs(400 * 24 * 60 * 60 * 1000), NOW)).toBe("a year ago");
  });

  it("plural years past two years", () => {
    expect(fmtRelativePast(isoMinusMs(800 * 24 * 60 * 60 * 1000), NOW)).toBe("2 years ago");
  });

  it("future timestamps degrade to 'today' rather than throwing", () => {
    // Clock-skew defence: server clock ahead of client. Don't crash the
    // page; bucket as 'today' which is at least true-ish.
    expect(fmtRelativePast(new Date(NOW + 60_000).toISOString(), NOW)).toBe("today");
  });
});

// Sanity smoke for the helpers that already shipped — guards against
// the new file accidentally re-exporting names.
describe("existing helpers still exported", () => {
  it("fmtDuration / fmtMs / fmtBytes return strings", () => {
    expect(typeof fmtDuration(60)).toBe("string");
    expect(typeof fmtMs(50)).toBe("string");
    expect(typeof fmtBytes(1024)).toBe("string");
  });
});
