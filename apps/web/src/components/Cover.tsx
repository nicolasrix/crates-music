// Single source of truth for "render an album/artist/track cover, with a
// graceful fallback when the image is missing or fails to load."
//
// Two missing-art failure modes the rest of the app used to handle in
// ad-hoc ways:
//
//   1) The Subsonic record has no `coverArt` field — known up front,
//      placeholder rendered synchronously.
//   2) `coverArt` is set but `/rest/getCoverArt` returns 404 / empty —
//      we only learn after the request fails. The `<img>`'s `onError`
//      flips us to the placeholder branch.
//
// Component intentionally renders *inner content only* (fills 100% of
// its parent). Existing wrappers like `.tile-cover`, `.search-hero-cover`,
// `.np .cover`, `.now-playing-cover`, `.hero .cover-lg`, `.suggestion-cover`
// keep their sizing + border-radius; this component just swaps the
// img / placeholder inside them. That's why the migration is small.
//
// Placeholder is a deterministic SVG: gradient + initial. The hue comes
// from a hash of the seed, so the same album always gets the same color
// across reloads and the page doesn't shimmer.

import { useEffect, useId, useState } from "react";
import { coverArtUrl } from "../api/client";

interface CoverProps {
  /** Subsonic cover-art id. Undefined / empty → placeholder. */
  coverArt: string | undefined;
  /** Stable identifier for the placeholder hash + initial. Album name,
   *  track title, artist name. Falls back to the Subsonic id when the
   *  display string is itself missing. */
  seed: string;
  /** Pixel size to request from the gateway. Doesn't constrain rendered
   *  size — the parent's CSS does that. */
  size: number;
  /** `<img alt>` and the placeholder's accessible label. */
  alt: string;
  loading?: "lazy" | "eager";
}

export function Cover({
  coverArt,
  seed,
  size,
  alt,
  loading = "lazy",
}: CoverProps) {
  const [errored, setErrored] = useState(false);
  // Reset the error gate when the cover identity changes — otherwise a
  // single 404 would pin the placeholder for any subsequent track that
  // reuses this component instance.
  useEffect(() => {
    setErrored(false);
  }, [coverArt]);

  // Pass `seed` through to the gateway: when Navidrome returns its default
  // placeholder image (a music note for albums, a person/star icon for
  // artists — distinct from a 404), the gateway substitutes our SVG and
  // uses this seed to pick the initial. Without it, every artist tile
  // would show the first letter of its opaque cover-art id ("A" for "ar-…").
  const url = coverArtUrl(coverArt, size, seed);
  if (!url || errored) {
    return <Placeholder seed={seed} alt={alt} />;
  }
  return (
    <img
      className="cover-img"
      src={url}
      alt={alt}
      loading={loading}
      onError={() => setErrored(true)}
    />
  );
}

function Placeholder({ seed, alt }: { seed: string; alt: string }) {
  const gradId = useId();
  const { c1, c2 } = placeholderColors(seed);
  const initial = initialOf(seed);
  // Decorative variant: when the consumer passes an empty alt, the cover
  // is non-informative (e.g. PlayerBar where the title sits immediately
  // adjacent). Skip the role="img" + aria-label to avoid announcing
  // "image" twice; use aria-hidden so screen readers ignore it.
  const decorative = alt === "";
  const a11y = decorative
    ? { "aria-hidden": true as const }
    : { role: "img" as const, "aria-label": alt };
  // viewBox 0..100 + preserveAspectRatio="xMidYMid slice" makes the
  // gradient fill any aspect ratio without distortion. Text is sized
  // in viewBox units so it scales with the parent automatically.
  return (
    <svg
      className="cover-img cover-placeholder"
      viewBox="0 0 100 100"
      preserveAspectRatio="xMidYMid slice"
      {...a11y}
    >
      <defs>
        <linearGradient id={gradId} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0%" stopColor={c1} />
          <stop offset="100%" stopColor={c2} />
        </linearGradient>
      </defs>
      <rect width="100" height="100" fill={`url(#${gradId})`} />
      <text
        x="50"
        y="50"
        textAnchor="middle"
        dominantBaseline="central"
        fill="rgba(255,255,255,0.92)"
        fontFamily="system-ui, -apple-system, Segoe UI, Roboto, sans-serif"
        fontWeight="700"
        fontSize="44"
        // Some browsers nudge `dominantBaseline="central"` 1–2 units off
        // the visual centre with bold weights — `dy=".05em"` hides that.
        dy=".05em"
      >
        {initial}
      </text>
    </svg>
  );
}

/**
 * FNV-1a 32-bit hash of a string. Cheap, deterministic, and good enough
 * for "spread inputs across a hue wheel" — we don't need cryptographic
 * properties.
 */
export function hashSeed(seed: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < seed.length; i++) {
    h ^= seed.charCodeAt(i);
    // 32-bit FNV prime multiplication via shifts to stay in int range.
    h = (h + ((h << 1) + (h << 4) + (h << 7) + (h << 8) + (h << 24))) >>> 0;
  }
  return h;
}

/**
 * Pick two HSL stops for the placeholder gradient. The base hue rotates
 * by the golden angle (137.508°) per integer hash unit, which distributes
 * adjacent hashes (e.g. "Pink Floyd" vs "Pink Martini") to far-apart
 * hues instead of clustering. Lightness/saturation are fixed so the
 * placeholders read as a coherent set rather than a random palette.
 */
export function placeholderColors(seed: string): { c1: string; c2: string } {
  const h = hashSeed(seed || "?");
  const hue = (h * 137.508) % 360;
  const c1 = `hsl(${hue.toFixed(1)}, 55%, 38%)`;
  const c2 = `hsl(${((hue + 28) % 360).toFixed(1)}, 55%, 22%)`;
  return { c1, c2 };
}

/**
 * First grapheme of the seed, uppercased. Falls back to "?" for empty /
 * non-letter seeds (rare — typically a bare-id artist with no name).
 */
export function initialOf(seed: string): string {
  const trimmed = (seed ?? "").trim();
  if (trimmed.length === 0) return "?";
  // Use Intl.Segmenter when available so emoji / combined glyphs / non-Latin
  // scripts work. Falls back to the first code unit for older browsers,
  // which is fine for ASCII names.
  if (typeof Intl !== "undefined" && "Segmenter" in Intl) {
    const seg = new Intl.Segmenter(undefined, { granularity: "grapheme" });
    const first = seg.segment(trimmed)[Symbol.iterator]().next().value;
    return (first?.segment ?? trimmed[0]!).toUpperCase();
  }
  return trimmed[0]!.toUpperCase();
}
