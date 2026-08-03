import { describe, expect, it } from "vitest";

import { RECONNECT_BASE_MS, RECONNECT_MAX_MS, reconnectDelayMs } from "./reconnect";

// `random = () => 1` picks the top of the jitter window, `() => 0` the
// bottom — so the two together pin the exact bounds of each attempt.
const top = () => 1;
const bottom = () => 0;

describe("reconnectDelayMs", () => {
  it("starts fast so a momentary blip is invisible", () => {
    expect(reconnectDelayMs(0, top)).toBe(RECONNECT_BASE_MS);
    expect(reconnectDelayMs(0, bottom)).toBe(RECONNECT_BASE_MS / 2);
  });

  it("doubles per attempt", () => {
    expect(reconnectDelayMs(1, top)).toBe(RECONNECT_BASE_MS * 2);
    expect(reconnectDelayMs(2, top)).toBe(RECONNECT_BASE_MS * 4);
    expect(reconnectDelayMs(3, top)).toBe(RECONNECT_BASE_MS * 8);
  });

  it("caps, and stays capped for an arbitrarily long outage", () => {
    // A tab left in a tunnel overnight must not drive 2**attempt to
    // Infinity, and must still be probing at least twice a minute.
    for (const attempt of [10, 50, 1000, Number.MAX_SAFE_INTEGER]) {
      expect(reconnectDelayMs(attempt, top)).toBe(RECONNECT_MAX_MS);
      expect(reconnectDelayMs(attempt, bottom)).toBe(RECONNECT_MAX_MS / 2);
    }
  });

  it("jitters so devices that dropped together don't return together", () => {
    // Every draw lands inside its window, and the window is genuinely
    // half-open — two different draws give two different delays.
    const lo = reconnectDelayMs(5, bottom);
    const hi = reconnectDelayMs(5, top);
    expect(lo).toBeLessThan(hi);
    expect(reconnectDelayMs(5, () => 0.5)).toBeGreaterThan(lo);
    expect(reconnectDelayMs(5, () => 0.5)).toBeLessThan(hi);
  });

  it("treats a negative attempt as the first one", () => {
    expect(reconnectDelayMs(-3, top)).toBe(RECONNECT_BASE_MS);
  });
});
