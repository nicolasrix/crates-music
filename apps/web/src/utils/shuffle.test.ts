import { afterEach, describe, expect, it, vi } from "vitest";

import { shuffle } from "./shuffle";

describe("shuffle", () => {
  afterEach(() => vi.restoreAllMocks());

  it("returns a new array and does not mutate the input", () => {
    const input = [1, 2, 3, 4, 5];
    const snapshot = [...input];
    const out = shuffle(input);
    expect(out).not.toBe(input);
    expect(input).toEqual(snapshot); // input untouched
  });

  it("is a permutation — same elements, same length", () => {
    const input = ["a", "b", "c", "d", "e", "f"];
    const out = shuffle(input);
    expect(out).toHaveLength(input.length);
    expect([...out].sort()).toEqual([...input].sort());
  });

  it("handles empty and single-element inputs", () => {
    expect(shuffle([])).toEqual([]);
    expect(shuffle([42])).toEqual([42]);
  });

  it("produces the Fisher-Yates order for a fixed RNG", () => {
    // Math.random returns 0 → j is always 0 at each step: each i swaps with
    // index 0. Walking [0,1,2,3] through that yields a deterministic order,
    // which pins the algorithm (not just 'some permutation').
    vi.spyOn(Math, "random").mockReturnValue(0);
    expect(shuffle([0, 1, 2, 3])).toEqual([1, 2, 3, 0]);
  });
});
