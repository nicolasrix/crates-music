import { describe, expect, it } from "vitest";
import { isDislikedEntity, nextPlayableIndex, type DislikeMap } from "./autoSkip";

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

describe("isDislikedEntity", () => {
  const maps = (
    tracks: [string, "like" | "dislike"][],
    albums: [string, "like" | "dislike"][],
    artists: [string, "like" | "dislike"][],
  ): { tracks: DislikeMap; albums: DislikeMap; artists: DislikeMap } => ({
    tracks: new Map(tracks),
    albums: new Map(albums),
    artists: new Map(artists),
  });

  it("is false for an undefined track id", () => {
    expect(isDislikedEntity(undefined, undefined, maps([], [], []))).toBe(false);
  });

  it("detects a directly disliked track", () => {
    expect(
      isDislikedEntity("t1", { albumId: "al1", artistId: "ar1" }, maps([["t1", "dislike"]], [], [])),
    ).toBe(true);
  });

  it("detects a track whose album is disliked", () => {
    expect(
      isDislikedEntity("t1", { albumId: "al1", artistId: "ar1" }, maps([], [["al1", "dislike"]], [])),
    ).toBe(true);
  });

  it("detects a track whose artist is disliked", () => {
    expect(
      isDislikedEntity("t1", { albumId: "al1", artistId: "ar1" }, maps([], [], [["ar1", "dislike"]])),
    ).toBe(true);
  });

  it("is false when the track and its (known) parents are all neutral or liked", () => {
    expect(
      isDislikedEntity(
        "t1",
        { albumId: "al1", artistId: "ar1" },
        maps([["t1", "like"]], [["al1", "like"]], []),
      ),
    ).toBe(false);
  });

  it("ignores parent dislikes when the parent ids are unknown (unhydrated)", () => {
    // A disliked album, but this queue item isn't hydrated so we don't know
    // its album_id — it can't be skipped on lookahead (converges on landing).
    expect(isDislikedEntity("t1", undefined, maps([], [["al1", "dislike"]], []))).toBe(false);
  });
});
