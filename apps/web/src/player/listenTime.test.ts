import { describe, expect, it } from "vitest";

import { FRESH_CLOCK, tick, sourceAtCursor } from "./listenTime";

function play(...positions: number[]) {
  return positions.reduce(tick, FRESH_CLOCK);
}

describe("tick", () => {
  it("counts steady forward playback", () => {
    expect(play(0, 250, 500, 750, 1000).heardMs).toBe(1000);
  });

  it("starts counting from the first position it sees, not from zero", () => {
    // Resumed at 60s: the minute before wasn't heard on this load.
    expect(play(60_000, 60_250, 60_500).heardMs).toBe(500);
  });

  it("does not count a forward seek", () => {
    expect(play(0, 1000, 90_000, 91_000).heardMs).toBe(2000);
  });

  it("does not count a backward seek, but counts the replay after it", () => {
    // 0→2000 heard (2s), jump back to 500 (not counted), 500→1500 heard (1s).
    expect(play(0, 1000, 2000, 500, 1500).heardMs).toBe(3000);
  });

  it("leaves the input clock untouched", () => {
    const before = play(0, 250);
    tick(before, 500);
    expect(before.heardMs).toBe(250);
  });
});

describe("sourceAtCursor", () => {
  const album = { kind: "album", id: "al-1" };
  const items = [
    { item_id: "a", track_id: "t1", source: album },
    { item_id: "b", track_id: "t2" },
  ];

  it("returns the cursor item's source when it holds the track", () => {
    expect(sourceAtCursor(items, 0, "t1")).toEqual(album);
  });

  it("returns undefined when the cursor holds another track", () => {
    expect(sourceAtCursor(items, 1, "t1")).toBeUndefined();
    expect(sourceAtCursor(items, null, "t1")).toBeUndefined();
  });
});
