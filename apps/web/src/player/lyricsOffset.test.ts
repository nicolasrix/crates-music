import { beforeEach, describe, expect, it } from "vitest";

import {
  MAX_REMEMBERED,
  OFFSET_LIMIT_MS,
  OFFSET_STEP_MS,
  formatOffset,
  loadOffsets,
  normalizeOffsets,
  readOffset,
  withOffset,
  writeOffset,
} from "./lyricsOffset";

describe("normalizeOffsets", () => {
  it("rejects non-objects rather than throwing", () => {
    expect(normalizeOffsets(null)).toEqual({});
    expect(normalizeOffsets("nope")).toEqual({});
    expect(normalizeOffsets([1, 2])).toEqual({});
  });

  it("drops non-numeric and non-finite values", () => {
    expect(normalizeOffsets({ a: "500", b: NaN, c: 500 })).toEqual({ c: 500 });
  });

  it("drops zeroes so they never occupy a remembered slot", () => {
    expect(normalizeOffsets({ a: 0, b: 250 })).toEqual({ b: 250 });
  });

  it("clamps to the limit in both directions", () => {
    expect(normalizeOffsets({ a: 99_000 })).toEqual({ a: OFFSET_LIMIT_MS });
    expect(normalizeOffsets({ a: -99_000 })).toEqual({ a: -OFFSET_LIMIT_MS });
  });

  it("snaps an off-step value onto the step grid", () => {
    // Otherwise a hand-edited 137 could never be walked back to zero by the
    // ± buttons, which only ever move in whole steps.
    expect(normalizeOffsets({ a: 137 })).toEqual({ a: OFFSET_STEP_MS });
  });
});

describe("withOffset", () => {
  it("returns a new map without mutating the original", () => {
    const before = { a: 250 };
    const after = withOffset(before, "b", 500);
    expect(before).toEqual({ a: 250 });
    expect(after).toEqual({ a: 250, b: 500 });
  });

  it("removes the entry when nudged back to zero", () => {
    expect(withOffset({ a: 250, b: 500 }, "a", 0)).toEqual({ b: 500 });
  });

  it("moves an updated track to the young end of the ring", () => {
    const m = withOffset(withOffset({}, "a", 250), "b", 250);
    expect(Object.keys(withOffset(m, "a", 500))).toEqual(["b", "a"]);
  });

  it("prunes the oldest entry once the ring is full", () => {
    let m = {};
    for (let i = 0; i < MAX_REMEMBERED; i++) m = withOffset(m, `t${i}`, 250);
    expect(Object.keys(m)).toHaveLength(MAX_REMEMBERED);

    m = withOffset(m, "newest", 250);
    const keys = Object.keys(m);
    expect(keys).toHaveLength(MAX_REMEMBERED);
    expect(keys).not.toContain("t0");
    expect(keys).toContain("newest");
  });
});

// Vitest runs in the `node` environment here (see vite.config.ts) so there
// is no localStorage. A five-line in-memory shim is cheaper than pulling in
// jsdom for one describe block, and it exercises the real load/save path
// rather than a mock of it.
function installLocalStorage(): void {
  const store = new Map<string, string>();
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
      removeItem: (k: string) => void store.delete(k),
      clear: () => store.clear(),
    },
  });
}

describe("persistence", () => {
  beforeEach(() => {
    installLocalStorage();
  });

  it("reads back what it wrote", () => {
    writeOffset("t1", 500);
    expect(readOffset("t1")).toBe(500);
  });

  it("returns zero for an untouched track", () => {
    expect(readOffset("t1")).toBe(0);
  });

  it("returns the clamped value actually stored, not the requested one", () => {
    expect(writeOffset("t1", 99_000)).toBe(OFFSET_LIMIT_MS);
  });

  it("returns zero and forgets the track when nudged back to zero", () => {
    writeOffset("t1", 250);
    expect(writeOffset("t1", 0)).toBe(0);
    expect(loadOffsets()).toEqual({});
  });

  it("survives a corrupt blob", () => {
    localStorage.setItem("crates-music.lyrics.offsets", "{not json");
    expect(loadOffsets()).toEqual({});
  });
});

describe("formatOffset", () => {
  it("is empty at zero", () => {
    expect(formatOffset(0)).toBe("");
  });

  it("signs both directions", () => {
    expect(formatOffset(250)).toBe("+0.25s");
    expect(formatOffset(-500)).toBe("−0.5s");
  });
});
