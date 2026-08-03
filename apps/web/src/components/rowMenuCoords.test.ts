import { describe, expect, it } from "vitest";
import { MENU_H_GUESS, MENU_W, menuCoords } from "./rowMenuCoords";

const VIEWPORT = { width: 1280, height: 900 };

describe("menuCoords", () => {
  it("right-aligns the panel to the trigger and opens downward", () => {
    const c = menuCoords({ top: 100, bottom: 128, right: 800 }, VIEWPORT);
    expect(c).toEqual({ left: 800 - MENU_W, top: 132 });
  });

  it("flips above when the panel wouldn't fit below", () => {
    // Trigger sits low enough that bottom + gap + guess exceeds the height.
    const top = VIEWPORT.height - MENU_H_GUESS;
    const c = menuCoords({ top, bottom: top + 28, right: 800 }, VIEWPORT);
    // Bottom-anchored: measured up from the viewport's bottom edge to the
    // trigger's top, so the panel's own height never enters the maths.
    expect(c).toEqual({ left: 800 - MENU_W, bottom: VIEWPORT.height - top + 4 });
  });

  it("clamps against the right edge", () => {
    const c = menuCoords(
      { top: 10, bottom: 38, right: VIEWPORT.width },
      VIEWPORT,
    );
    expect(c.left).toBe(VIEWPORT.width - MENU_W - 8);
  });

  it("clamps against the left edge for a trigger near x=0", () => {
    const c = menuCoords({ top: 10, bottom: 38, right: 40 }, VIEWPORT);
    expect(c.left).toBe(8);
  });

  it("keeps the panel on-screen at a normal phone width", () => {
    // 320px: right-aligning to a trigger at x=300 leaves the panel fully
    // inside the viewport, so neither clamp should move it.
    const c = menuCoords({ top: 10, bottom: 38, right: 300 }, { width: 320, height: 640 });
    expect(c.left).toBe(300 - MENU_W);
  });

  it("never goes negative on a viewport narrower than the panel", () => {
    // Under MENU_W + 2·EDGE the right-edge clamp alone computes a
    // negative left. The left floor has to win — the panel then
    // overflows to the right, where `.row-menu`'s own 100vw - 16px
    // width cap catches it.
    const c = menuCoords({ top: 10, bottom: 38, right: 190 }, { width: 200, height: 640 });
    expect(c.left).toBe(8);
  });

  it("floors `bottom` for a trigger scrolled past the viewport edge", () => {
    // A row can be dragged/scrolled below the fold between the click and
    // the measure. Without the floor this yields a negative `bottom`,
    // which parks the panel entirely off-screen.
    const c = menuCoords({ top: 910, bottom: 938, right: 800 }, VIEWPORT);
    expect(c).toEqual({ left: 800 - MENU_W, bottom: 8 });
  });
});
