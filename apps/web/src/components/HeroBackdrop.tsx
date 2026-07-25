// Blurred blow-up of the cover art filling the hero — the artwork itself
// becomes the backdrop, so multi-color covers contribute every color they
// have, not just the two hues the palette extractor kept. Legibility is
// the CSS scrim's job (.hero-backdrop::after re-pins lightness toward
// --art-bg), not the image's; the synthesized glow radials underneath
// remain the base while the image loads, and the whole story for pages
// with no artwork.
//
// `url` should be the same coverArtUrl(...) the page already uses for its
// hero <Cover> and palette extraction, so this adds zero network cost —
// the browser serves it from cache.
export function HeroBackdrop({ url }: { url: string | null }) {
  if (!url) return null;
  return (
    <div className="hero-backdrop" aria-hidden="true">
      <img src={url} alt="" loading="eager" decoding="async" />
    </div>
  );
}
