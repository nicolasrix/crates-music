import { describe, expect, it } from "vitest";
import {
  ACCENT_MIN_CHROMA,
  CHROMA_CAP,
  synthesizeArtPalette,
} from "./artPalette";
import { hexToOklch, hueDistanceDeg } from "./oklch";

describe("synthesizeArtPalette", () => {
  it("returns null for no colors", () => {
    expect(synthesizeArtPalette([])).toBeNull();
  });

  it("lets a small saturated subject out-vote a large grey background", () => {
    const p = synthesizeArtPalette([
      { hex: "#808080", area: 0.8 }, // grey backdrop
      { hex: "#cc3322", area: 0.2 }, // red subject
    ])!;
    const red = hexToOklch("#cc3322")!;
    expect(hueDistanceDeg(p.hue, red.h)).toBeLessThan(5);
    expect(p.hasAccent).toBe(true);
  });

  it("flags greyscale covers as accent-less", () => {
    const p = synthesizeArtPalette([
      { hex: "#222222", area: 0.5 },
      { hex: "#bbbbbb", area: 0.5 },
    ])!;
    expect(p.hasAccent).toBe(false);
    expect(p.chroma).toBeLessThan(ACCENT_MIN_CHROMA);
  });

  it("caps chroma so neon covers can't shout", () => {
    const p = synthesizeArtPalette([{ hex: "#ff0000", area: 1 }])!;
    expect(p.chroma).toBe(CHROMA_CAP);
  });

  it("admits a secondary hue only when clearly distinct", () => {
    const redBlue = synthesizeArtPalette([
      { hex: "#cc3322", area: 0.6 },
      { hex: "#2244cc", area: 0.4 },
    ])!;
    const blue = hexToOklch("#2244cc")!;
    expect(hueDistanceDeg(redBlue.hue2, blue.h)).toBeLessThan(5);

    // Red + orange are analogous — too close, secondary collapses to primary.
    const redOrange = synthesizeArtPalette([
      { hex: "#cc3322", area: 0.6 },
      { hex: "#cc7722", area: 0.4 },
    ])!;
    expect(redOrange.hue2).toBe(redOrange.hue);
  });

  it("ignores unparseable hex entries", () => {
    const p = synthesizeArtPalette([
      { hex: "not-a-color", area: 0.9 },
      { hex: "#2244cc", area: 0.1 },
    ])!;
    const blue = hexToOklch("#2244cc")!;
    expect(hueDistanceDeg(p.hue, blue.h)).toBeLessThan(5);
  });
});
