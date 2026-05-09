// Per-page artwork palette. Detail pages (album / artist / playlist) extract
// four hex values from the cover and write them as CSS custom properties on
// `<main>`. Components downstream (scrubber fill, hero play disc, currently-
// playing row) read `var(--art-accent)` etc. with a fallback to the chrome
// accent — see design_handoff_crates_web/README.md "central mechanism".
//
// Why a context: the palette source (the album/artist page) and one of its
// consumers (the player bar) live in different subtrees, so we lift the
// active palette to a provider and let the player bar subscribe.

import { createContext, ReactNode, useContext, useEffect, useMemo, useState } from "react";
import { extractColors } from "extract-colors";
import { useQuery } from "@tanstack/react-query";

export interface Palette {
  bg: string;
  fg: string;
  mute: string;
  accent: string;
}

interface Ctx {
  palette: Palette | null;
  setPalette: (p: Palette | null) => void;
}

const ArtworkCtx = createContext<Ctx | null>(null);

export function ArtworkProvider({ children }: { children: ReactNode }) {
  const [palette, setPalette] = useState<Palette | null>(null);
  const value = useMemo(() => ({ palette, setPalette }), [palette]);
  return <ArtworkCtx.Provider value={value}>{children}</ArtworkCtx.Provider>;
}

export function useArtwork(): Ctx {
  const v = useContext(ArtworkCtx);
  if (!v) throw new Error("useArtwork must be used inside <ArtworkProvider>");
  return v;
}

/** Apply a palette to the closest `<main>` ancestor by writing the four
 *  --art-* variables. Reverts on unmount so navigating back to a chrome-only
 *  page (e.g. /diagnostics) doesn't leak the previous album's tint. */
export function useArtworkOnMain(palette: Palette | null) {
  useEffect(() => {
    const main = document.querySelector("main");
    if (!main) return;
    if (!palette) {
      main.style.removeProperty("--art-bg");
      main.style.removeProperty("--art-fg");
      main.style.removeProperty("--art-mute");
      main.style.removeProperty("--art-accent");
      return;
    }
    main.style.setProperty("--art-bg", palette.bg);
    main.style.setProperty("--art-fg", palette.fg);
    main.style.setProperty("--art-mute", palette.mute);
    main.style.setProperty("--art-accent", palette.accent);
    return () => {
      main.style.removeProperty("--art-bg");
      main.style.removeProperty("--art-fg");
      main.style.removeProperty("--art-mute");
      main.style.removeProperty("--art-accent");
    };
  }, [palette]);
}

/** Extract a 4-color palette from a cover image URL. Cached by URL via
 *  TanStack Query — we only run extract-colors once per album. */
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

async function extractFromUrl(url: string): Promise<Palette> {
  // extract-colors hits a canvas, so the image must be CORS-loadable — our
  // cover URLs come from /rest/getCoverArt on the same origin (the gateway
  // proxies same-origin in prod and Vite proxies in dev), so this works.
  const colors = await extractColors(url, { crossOrigin: "anonymous" });

  // Heuristic ordering — extract-colors returns colors sorted by area.
  // Pick:
  //   accent = the most-saturated color (chroma signal)
  //   bg     = darkest color (dark mode wash)
  //   fg     = lightest color (hero text)
  //   mute   = mid-luminance color (sub copy)
  // If the image is low-chroma (greyscale cover), accent falls back to the
  // brand amber via the consumer's `var(--art-accent, var(--accent))`.
  if (colors.length === 0) {
    return {
      bg: "var(--surface-0)",
      fg: "var(--fg)",
      mute: "var(--fg-muted)",
      accent: "var(--accent)",
    };
  }

  const byLightness = [...colors].sort((a, b) => a.lightness - b.lightness);
  const bySaturation = [...colors].sort((a, b) => b.saturation - a.saturation);

  const darkest = byLightness[0]!;
  const lightest = byLightness[byLightness.length - 1]!;
  const mid = byLightness[Math.floor(byLightness.length / 2)]!;
  const mostSat = bySaturation[0]!;

  return {
    bg: darkest.hex,
    fg: lightest.hex,
    mute: mid.hex,
    accent: mostSat.saturation > 0.15 ? mostSat.hex : "var(--accent)",
  };
}
