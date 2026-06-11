// Minimal sRGB-hex → OKLCH conversion. OKLCH is the app's working color
// space (all design tokens in tokens.css are oklch) — its lightness axis
// is perceptually uniform, which is what lets the artwork-tint guardrails
// pin text/background contrast by construction: two colors a fixed L apart
// read with the same contrast at any hue.
//
// Matrices are Björn Ottosson's reference OKLab constants.

export interface Oklch {
  /** Perceptual lightness, 0..1. */
  l: number;
  /** Chroma, 0..~0.37 for sRGB-representable colors. */
  c: number;
  /** Hue angle in degrees, 0..360. Meaningless when c ≈ 0. */
  h: number;
}

function srgbToLinear(u: number): number {
  return u <= 0.04045 ? u / 12.92 : Math.pow((u + 0.055) / 1.055, 2.4);
}

/** Parse `#rgb` / `#rrggbb` (case-insensitive, leading `#` optional).
 *  Returns null on anything else rather than guessing. */
export function hexToOklch(hex: string): Oklch | null {
  const raw = hex.startsWith("#") ? hex.slice(1) : hex;
  const expanded =
    raw.length === 3
      ? raw
          .split("")
          .map((ch) => ch + ch)
          .join("")
      : raw;
  if (!/^[0-9a-fA-F]{6}$/.test(expanded)) return null;

  const r = srgbToLinear(parseInt(expanded.slice(0, 2), 16) / 255);
  const g = srgbToLinear(parseInt(expanded.slice(2, 4), 16) / 255);
  const b = srgbToLinear(parseInt(expanded.slice(4, 6), 16) / 255);

  const lms0 = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const lms1 = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const lms2 = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);

  const L = 0.2104542553 * lms0 + 0.793617785 * lms1 - 0.0040720468 * lms2;
  const a = 1.9779984951 * lms0 - 2.428592205 * lms1 + 0.4505937099 * lms2;
  const bb = 0.0259040371 * lms0 + 0.7827717662 * lms1 - 0.808675766 * lms2;

  const c = Math.hypot(a, bb);
  const h = ((Math.atan2(bb, a) * 180) / Math.PI + 360) % 360;
  return { l: L, c, h };
}

/** Shortest angular distance between two hues, 0..180 degrees. */
export function hueDistanceDeg(a: number, b: number): number {
  const d = Math.abs(a - b) % 360;
  return d > 180 ? 360 - d : d;
}
