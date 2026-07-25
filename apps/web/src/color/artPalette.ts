// Artwork palette synthesis. We deliberately do NOT use raw cover colors
// in the UI — a navy "accent" is invisible on the dark theme, a neon one
// shouts, and four unrelated picks clash. Instead the cover contributes
// only its *identity* (hue + a capped chroma); every visible role
// (wash, hero text, accent) is re-synthesized in CSS at fixed OKLCH
// lightness bands per theme (see the `.has-art` rules in tokens.css).
// Legibility is then guaranteed by construction, not by luck.

import { hexToOklch, hueDistanceDeg } from "./oklch";

export interface ArtPalette {
  /** Dominant chromatic hue of the cover (OKLCH degrees). */
  hue: number;
  /** Chroma of that color, capped at CHROMA_CAP. */
  chroma: number;
  /** A clearly-distinct secondary hue (≥ MIN_HUE_GAP_DEG away) for
   *  two-tone washes; equals `hue` when the cover doesn't have one —
   *  "colors work together, or are distanced enough", never in between. */
  hue2: number;
  /** False for (near-)greyscale covers: tint washes stay neutral and the
   *  accent falls back to the brand amber instead of a synthesized grey. */
  hasAccent: boolean;
}

/** The subset of an extract-colors result we consume. */
export interface ExtractedColor {
  hex: string;
  /** Fraction of the image this color covers, 0..1. */
  area: number;
}

/** Accent chroma ceiling — keeps neon covers from producing a shouting
 *  accent. Slightly above the brand amber's 0.13 so vivid covers still
 *  read as more colorful than chrome. */
export const CHROMA_CAP = 0.15;
/** Below this the color carries no usable hue signal (greyscale). */
export const ACCENT_MIN_CHROMA = 0.05;
/** Minimum hue separation before a second hue is admitted at all. */
export const MIN_HUE_GAP_DEG = 50;

/** Down-weight very dark / very light pixels clusters: their hue is
 *  numerically unstable (c → 0 near the gamut tips), so a huge near-black
 *  background shouldn't out-vote a smaller saturated subject. Peaks at
 *  mid-lightness, fades to 0 at the extremes. */
function lightnessWeight(l: number): number {
  return Math.max(0, 1 - Math.abs(l - 0.6) / 0.6);
}

export function synthesizeArtPalette(
  colors: readonly ExtractedColor[],
): ArtPalette | null {
  const scored = colors
    .flatMap((col) => {
      const ok = hexToOklch(col.hex);
      return ok ? [{ ...ok, score: col.area * ok.c * lightnessWeight(ok.l) }] : [];
    })
    .sort((a, b) => b.score - a.score);
  if (scored.length === 0) return null;

  const primary = scored[0]!;
  const secondary = scored.find(
    (cand) =>
      cand !== primary &&
      cand.c >= ACCENT_MIN_CHROMA &&
      hueDistanceDeg(cand.h, primary.h) >= MIN_HUE_GAP_DEG,
  );

  return {
    hue: primary.h,
    chroma: Math.min(primary.c, CHROMA_CAP),
    hue2: secondary ? secondary.h : primary.h,
    hasAccent: primary.c >= ACCENT_MIN_CHROMA,
  };
}
