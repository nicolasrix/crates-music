// Where a ⋯-menu panel gets pinned, given its trigger's bounding box.
//
// Split out of the menu component because the cases that matter — a
// trigger near the bottom of the viewport, a trigger hard against the
// right edge, a phone narrower than the panel — are precisely the ones
// that are painful to reproduce by hand in a browser but trivial to
// assert on as arithmetic.

/** Panel width. Keep in sync with `.row-menu`'s `width` in components.css. */
export const MENU_W = 220;

/** Only drives the flip-above decision, never the rendered geometry.
 *  Erring high is safe (the menu flips a little eagerly, and a
 *  bottom-anchored panel needs no height at all); erring low strands the
 *  last entries off-screen — so this tracks the *tallest* menu we render
 *  (the album menu's ~9 entries plus separators), not the smallest. */
export const MENU_H_GUESS = 380;

/** Minimum gap kept between the panel and the viewport edge. */
const EDGE = 8;
/** Gap between the trigger and the panel. */
const GAP = 4;

/** The parts of a `DOMRect` placement actually reads. */
export interface TriggerRect {
  top: number;
  bottom: number;
  right: number;
}

export interface Viewport {
  width: number;
  height: number;
}

/** Top-anchored coords grow the panel downward; bottom-anchored coords
 *  grow it upward. Anchoring by `bottom` on the flip is what lets us
 *  place the panel without knowing its real height — which varies with
 *  the entry count (3 in the player, ~9 in the album menu) and so can't
 *  be predicted from a single constant. */
export type MenuCoords =
  | { left: number; top: number }
  | { left: number; bottom: number };

export function menuCoords(rect: TriggerRect, viewport: Viewport): MenuCoords {
  // Right-align the panel to the trigger, pull it back off the right
  // edge, then floor at the left edge. The floor has to be applied
  // *last*: on a viewport narrower than MENU_W + 2·EDGE the right-edge
  // clamp alone goes negative and parks the panel off-screen to the
  // left. Overflowing to the right instead is safe — `.row-menu` caps
  // its own width at 100vw - 16px.
  const left = Math.max(
    EDGE,
    Math.min(viewport.width - MENU_W - EDGE, rect.right - MENU_W),
  );
  const overflowsBelow = rect.bottom + GAP + MENU_H_GUESS > viewport.height;
  return overflowsBelow
    ? { left, bottom: Math.max(EDGE, viewport.height - rect.top + GAP) }
    : { left, top: rect.bottom + GAP };
}
