// A neighbor crate, rendered small and dimmed beside the active digger:
// the previous crate peeks in from the left, the next from the right, so
// "there are more crates on this table" is visible instead of implied by
// a menu. Clicking one (or swiping the main stage horizontally) moves to
// it. Non-interactive beyond that — a static stack of the crate's first
// few sleeves reusing the same .crate-box / .sleeve CSS at a smaller
// --sleeve-size (overridden in the .crate-preview block).

import type { CSSProperties } from "react";
import type { Album } from "../api/types";
import { Cover } from "../components/Cover";
import type { Crate } from "./buildCrates";

/** How many sleeves a preview stack shows. */
const PREVIEW_SLEEVES = 5;

interface CratePreviewProps {
  crate: Crate;
  side: "left" | "right";
  onSelect: () => void;
}

export function CratePreview({ crate, side, onSelect }: CratePreviewProps) {
  const covers: Album[] = crate.albums.slice(0, PREVIEW_SLEEVES);
  return (
    <button
      type="button"
      className={`crate-preview crate-preview-${side}`}
      aria-label={`${side === "left" ? "previous" : "next"} crate: ${crate.label}, ${crate.albums.length} records`}
      onClick={onSelect}
    >
      <span className="crate-box" aria-hidden="true">
        <span className="crate-box-back" />
        <span className="crate-box-front" />
        <span className="crate-tape">{crate.label}</span>
      </span>
      {covers.map((album, i) => (
        <span
          key={album.id}
          className="sleeve"
          aria-hidden="true"
          style={previewSleeveStyle(i)}
        >
          <Cover coverArt={album.coverArt} seed={album.name} size={300} alt="" />
        </span>
      ))}
    </button>
  );
}

/** A resting stack: front sleeve upright-ish, the rest leaning back the
 *  way the digger's behind-stack does, scaled to the preview's size. */
function previewSleeveStyle(i: number): CSSProperties {
  return {
    transform: `translate3d(0, ${-i * 13}px, ${-i * 24}px) rotateX(12deg)`,
    zIndex: 120 - i,
    opacity: 1 - i * 0.07,
  };
}

/** Placeholder keeping the carousel symmetric when there is no neighbor
 *  on one side (first/last crate). Carries the side class so the mobile
 *  edge-peek margins apply to it the same way. */
export function CratePreviewSpacer({ side }: { side: "left" | "right" }) {
  return (
    <span
      className={`crate-preview crate-preview-${side} is-empty`}
      aria-hidden="true"
    />
  );
}
