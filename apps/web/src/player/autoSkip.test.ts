import { describe, expect, it } from "vitest";
import { nextPlayableIndex } from "./autoSkip";

// Queue of 5; dislike a set of indices and walk from `start`.
function walk(
  start: number,
  direction: 1 | -1,
  total: number,
  disliked: number[],
) {
  const set = new Set(disliked);
  return nextPlayableIndex(start, direction, total, (i) => set.has(i));
}

describe("nextPlayableIndex", () => {
  it("returns the start index when it is not disliked", () => {
    expect(walk(2, 1, 5, [])).toBe(2);
  });

  it("skips forward past a run of disliked tracks", () => {
    // start on disliked 1; 2 and 3 also disliked; 4 is clean.
    expect(walk(1, 1, 5, [1, 2, 3])).toBe(4);
  });

  it("skips backward past disliked tracks", () => {
    expect(walk(3, -1, 5, [3, 2])).toBe(1);
  });

  it("returns null when every track ahead is disliked", () => {
    expect(walk(2, 1, 5, [2, 3, 4])).toBeNull();
  });

  it("returns null when every track behind is disliked", () => {
    expect(walk(2, -1, 5, [2, 1, 0])).toBeNull();
  });

  it("returns null on an empty queue", () => {
    expect(walk(0, 1, 0, [])).toBeNull();
  });

  it("stops at the forward bound rather than wrapping", () => {
    // last index disliked, nothing after → null (no wrap to the front).
    expect(walk(4, 1, 5, [4])).toBeNull();
  });
});
