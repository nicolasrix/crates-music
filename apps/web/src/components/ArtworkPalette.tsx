// Per-page artwork tint. Detail pages (album / artist / playlist) extract
// the cover's dominant hue + chroma and mount them as CSS custom properties
// (`--art-h`, `--art-c`, `--art-h2`) plus the `has-art` class on <main>;
// the player bar does the same for the *playing* track's cover on `.player`.
//
// The actual colors are synthesized in CSS — see the `.has-art` rules in
// tokens.css: each role (`--art-bg/fg/mute/accent`) is rebuilt at a fixed,
// theme-aware OKLCH lightness band with a per-role chroma cap. That is the
// legibility guardrail: the cover only contributes hue identity, never
// lightness, so text/background contrast is constant across every album
// and both themes (and theme switches re-tint live, no recompute).
//
// Greyscale covers set `has-art` without `has-art-accent`: washes stay
// neutral and accent roles keep the brand amber.

import { useEffect, type CSSProperties } from "react";
import { extractColors } from "extract-colors";
import { useQuery } from "@tanstack/react-query";
import { synthesizeArtPalette, type ArtPalette } from "../color/artPalette";

export type Palette = ArtPalette;

/** className + style pair that mounts a palette on any element (the player
 *  bar uses this; pages go through useArtworkOnMain). */
export function artAttrs(palette: Palette | null): {
  className: string;
  style: CSSProperties;
} {
  if (!palette) return { className: "", style: {} };
  return {
    className: palette.hasAccent ? "has-art has-art-accent" : "has-art",
    style: {
      ["--art-h" as never]: String(palette.hue),
      ["--art-c" as never]: String(palette.chroma),
      ["--art-h2" as never]: String(palette.hue2),
    },
  };
}

/** Mount a palette on the closest `<main>` (Layout calls this). Reverts on
 *  unmount so navigating to a chrome-only page (e.g. /settings) doesn't
 *  leak the previous album's tint. */
export function useArtworkOnMain(palette: Palette | null) {
  useEffect(() => {
    const main = document.querySelector("main");
    if (!main || !palette) return;
    main.classList.add("has-art");
    main.classList.toggle("has-art-accent", palette.hasAccent);
    main.style.setProperty("--art-h", String(palette.hue));
    main.style.setProperty("--art-c", String(palette.chroma));
    main.style.setProperty("--art-h2", String(palette.hue2));
    return () => {
      main.classList.remove("has-art", "has-art-accent");
      main.style.removeProperty("--art-h");
      main.style.removeProperty("--art-c");
      main.style.removeProperty("--art-h2");
    };
  }, [palette]);
}

/** Extract + synthesize a palette from a cover image URL. Cached by URL via
 *  TanStack Query — extract-colors runs once per cover, and the page and the
 *  player bar share the cache entry when they reference the same URL. */
export function useCoverPalette(coverUrl: string | null): Palette | null {
  const q = useQuery({
    queryKey: ["palette", coverUrl],
    queryFn: () => extractFromUrl(coverUrl!),
    enabled: !!coverUrl,
    // Palettes are stable per cover — no need to refetch.
    staleTime: Infinity,
    gcTime: Infinity,
    retry: false,
  });
  return q.data ?? null;
}

async function extractFromUrl(url: string): Promise<Palette | null> {
  // extract-colors hits a canvas, so the image must be CORS-loadable — our
  // cover URLs come from /rest/getCoverArt on the same origin (the gateway
  // proxies same-origin in prod and Vite proxies in dev), so this works.
  const colors = await extractColors(url, { crossOrigin: "anonymous" });
  return synthesizeArtPalette(
    colors.map((c) => ({ hex: c.hex, area: c.area })),
  );
}
