import { describe, expect, it } from "vitest";
import { hexToOklch, hueDistanceDeg } from "./oklch";

// Reference values from Björn Ottosson's OKLab post / CSS Color 4 samples.
describe("hexToOklch", () => {
  it("converts white to L≈1, C≈0", () => {
    const c = hexToOklch("#ffffff")!;
    expect(c.l).toBeCloseTo(1.0, 2);
    expect(c.c).toBeCloseTo(0, 2);
  });

  it("converts black to L≈0", () => {
    const c = hexToOklch("#000000")!;
    expect(c.l).toBeCloseTo(0, 2);
    expect(c.c).toBeCloseTo(0, 2);
  });

  it("converts pure red to the known OKLCH triple", () => {
    const c = hexToOklch("#ff0000")!;
    expect(c.l).toBeCloseTo(0.628, 2);
    expect(c.c).toBeCloseTo(0.258, 2);
    expect(c.h).toBeCloseTo(29.2, 0);
  });

  it("converts pure blue to the known OKLCH triple", () => {
    const c = hexToOklch("#0000ff")!;
    expect(c.l).toBeCloseTo(0.452, 2);
    expect(c.c).toBeCloseTo(0.313, 2);
    expect(c.h).toBeCloseTo(264.1, 0);
  });

  it("expands #rgb shorthand", () => {
    const short = hexToOklch("#f00")!;
    const long = hexToOklch("#ff0000")!;
    expect(short.l).toBeCloseTo(long.l, 6);
    expect(short.h).toBeCloseTo(long.h, 6);
  });

  it("accepts a missing # prefix and rejects garbage", () => {
    expect(hexToOklch("ff0000")).not.toBeNull();
    expect(hexToOklch("#zzzzzz")).toBeNull();
    expect(hexToOklch("#ffff")).toBeNull();
    expect(hexToOklch("")).toBeNull();
  });
});

describe("hueDistanceDeg", () => {
  it("measures across the 0/360 wrap", () => {
    expect(hueDistanceDeg(350, 10)).toBe(20);
    expect(hueDistanceDeg(10, 350)).toBe(20);
  });

  it("caps at 180", () => {
    expect(hueDistanceDeg(0, 180)).toBe(180);
    expect(hueDistanceDeg(90, 270)).toBe(180);
  });

  it("is zero for equal hues", () => {
    expect(hueDistanceDeg(123, 123)).toBe(0);
  });
});
