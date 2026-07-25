import { describe, expect, it } from "vitest";
import {
  DEFAULT_AUTOPLAY_SETTINGS,
  SETTINGS_BOUNDS,
  normalizeSettings,
} from "./autoplaySettings";

describe("normalizeSettings", () => {
  it("returns defaults for an empty / null input", () => {
    expect(normalizeSettings(undefined)).toEqual(DEFAULT_AUTOPLAY_SETTINGS);
    expect(normalizeSettings(null)).toEqual(DEFAULT_AUTOPLAY_SETTINGS);
    expect(normalizeSettings({})).toEqual(DEFAULT_AUTOPLAY_SETTINGS);
  });

  it("clamps out-of-range values to the field bounds", () => {
    const s = normalizeSettings({
      leashTau: 5, // > max 1
      leashLambda: -3, // < min 0
      frontierWeight: 100, // > max 3
      frontierDecay: -1, // < min 0
      mmrLambda: 2, // > max 1
    });
    expect(s.leashTau).toBe(SETTINGS_BOUNDS.leashTau.max);
    expect(s.leashLambda).toBe(SETTINGS_BOUNDS.leashLambda.min);
    expect(s.frontierWeight).toBe(SETTINGS_BOUNDS.frontierWeight.max);
    expect(s.frontierDecay).toBe(SETTINGS_BOUNDS.frontierDecay.min);
    expect(s.mmrLambda).toBe(SETTINGS_BOUNDS.mmrLambda.max);
  });

  it("rounds frontierWindow to an integer", () => {
    expect(normalizeSettings({ frontierWindow: 2.7 }).frontierWindow).toBe(3);
    expect(normalizeSettings({ frontierWindow: 2.2 }).frontierWindow).toBe(2);
  });

  it("falls back to the default for NaN / non-numeric fields", () => {
    const s = normalizeSettings({ leashTau: "nope", leashLambda: NaN });
    expect(s.leashTau).toBe(DEFAULT_AUTOPLAY_SETTINGS.leashTau);
    expect(s.leashLambda).toBe(DEFAULT_AUTOPLAY_SETTINGS.leashLambda);
  });

  it("preserves valid in-range values verbatim", () => {
    const s = normalizeSettings({
      leashTau: 0.3,
      leashLambda: 20,
      frontierWeight: 0.2,
      frontierDecay: 0.6,
      frontierWindow: 4,
      mmrLambda: 0.7,
    });
    expect(s).toEqual({
      leashTau: 0.3,
      leashLambda: 20,
      frontierWeight: 0.2,
      frontierDecay: 0.6,
      frontierWindow: 4,
      mmrLambda: 0.7,
    });
  });
});
