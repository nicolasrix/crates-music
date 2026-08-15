import { beforeEach, describe, expect, it, vi } from "vitest";

// Deterministic "shuffle": reverse. Randomness isn't what these tests are
// about — set membership and what survives a flip are — and a real
// Fisher-Yates would make every assertion a sort().
vi.mock("../utils/shuffle", () => ({
  shuffle: <T>(input: readonly T[]): T[] => [...input].reverse(),
}));

import {
  interleave,
  recommendationSlots,
  replanUpcoming,
  startOrder,
  type PlayContext,
} from "./playModePlan";

const CONTEXT: PlayContext = {
  sessionId: "sess-1",
  trackIds: ["a", "b", "c", "d", "e"],
};

describe("startOrder", () => {
  it("keeps the context order in in_order mode, anchored on the click", () => {
    // The regression this guards: anchoring at 0 would play track 1 when
    // the user clicked track 2.
    expect(startOrder("in_order", ["a", "b", "c"], 1)).toEqual({
      order: ["a", "b", "c"],
      anchorIndex: 1,
    });
  });

  it("puts the clicked track first and reorders the rest", () => {
    expect(startOrder("shuffle", ["a", "b", "c", "d"], 2)).toEqual({
      order: ["c", "d", "b", "a"],
      anchorIndex: 0,
    });
  });

  it("treats smart_shuffle the same as shuffle (recs are mixed in later)", () => {
    expect(startOrder("smart_shuffle", ["a", "b", "c"], 0)).toEqual({
      order: ["a", "c", "b"],
      anchorIndex: 0,
    });
  });

  it("clamps an out-of-range start index", () => {
    expect(startOrder("shuffle", ["a", "b"], 9).order).toEqual(["b", "a"]);
    expect(startOrder("shuffle", ["a", "b"], -3).order).toEqual(["a", "b"]);
  });

  it("handles an empty list", () => {
    expect(startOrder("shuffle", [], 0)).toEqual({ order: [], anchorIndex: 0 });
  });

  it("caps a huge context, keeping the pick and some history", () => {
    const big = Array.from({ length: 1000 }, (_, i) => `t${i}`);
    const { order, anchorIndex } = startOrder("in_order", big, 500, 100);
    expect(order).toHaveLength(100);
    expect(order[anchorIndex]).toBe("t500");
    expect(anchorIndex).toBe(20); // CAPPED_HISTORY tracks behind the pick
  });

  it("keeps the window inside the list at either end", () => {
    const big = Array.from({ length: 1000 }, (_, i) => `t${i}`);
    const atStart = startOrder("in_order", big, 3, 100);
    expect(atStart.order[0]).toBe("t0");
    expect(atStart.order[atStart.anchorIndex]).toBe("t3");
    const atEnd = startOrder("in_order", big, 999, 100);
    expect(atEnd.order).toHaveLength(100);
    expect(atEnd.order[atEnd.anchorIndex]).toBe("t999");
  });

  it("caps a shuffled context too, always keeping the pick", () => {
    const big = Array.from({ length: 1000 }, (_, i) => `t${i}`);
    const { order, anchorIndex } = startOrder("shuffle", big, 700, 100);
    expect(order).toHaveLength(100);
    expect(anchorIndex).toBe(0);
    expect(order[0]).toBe("t700");
  });
});

describe("replanUpcoming — with a known context", () => {
  it("shuffles everything not yet heard this session", () => {
    // Played a, now on c: b, d, e are unheard.
    const next = replanUpcoming({
      mode: "shuffle",
      context: CONTEXT,
      queueTrackIds: ["a", "c", "z"],
      nowPlayingIndex: 1,
    });
    expect(next).toEqual(["e", "d", "b"]); // reversed by the stub
  });

  it("never re-queues a track already behind the cursor", () => {
    const next = replanUpcoming({
      mode: "shuffle",
      context: CONTEXT,
      queueTrackIds: ["d", "a", "e", "b"],
      nowPlayingIndex: 2,
    });
    expect(next).not.toContain("d");
    expect(next).not.toContain("a");
    expect(next).not.toContain("e");
    expect(new Set(next)).toEqual(new Set(["b", "c"]));
  });

  it("restores context order after the current track when switching to in_order", () => {
    // Shuffle had thrown us to "b"; turning shuffle off continues c, d, e.
    const next = replanUpcoming({
      mode: "in_order",
      context: CONTEXT,
      queueTrackIds: ["e", "b", "a"],
      nowPlayingIndex: 1,
    });
    expect(next).toEqual(["c", "d"]); // "e" already heard, "a" is behind us
  });

  it("falls back to the whole unheard remainder when the current track isn't in the context", () => {
    // A mixed-in recommendation ("rec-1") is playing.
    const next = replanUpcoming({
      mode: "in_order",
      context: CONTEXT,
      queueTrackIds: ["a", "rec-1", "b"],
      nowPlayingIndex: 1,
    });
    expect(next).toEqual(["b", "c", "d", "e"]);
  });

  it("plans from the top when nothing is playing yet", () => {
    const next = replanUpcoming({
      mode: "in_order",
      context: CONTEXT,
      queueTrackIds: [],
      nowPlayingIndex: null,
    });
    expect(next).toEqual(["a", "b", "c", "d", "e"]);
  });
});

describe("replanUpcoming — context lost (other device, or a reload)", () => {
  it("can still shuffle what is already queued", () => {
    const next = replanUpcoming({
      mode: "shuffle",
      context: null,
      queueTrackIds: ["a", "b", "c", "d"],
      nowPlayingIndex: 0,
    });
    expect(next).toEqual(["d", "c", "b"]);
  });

  it("declines to restore an order it never saw", () => {
    expect(
      replanUpcoming({
        mode: "in_order",
        context: null,
        queueTrackIds: ["a", "b", "c"],
        nowPlayingIndex: 0,
      }),
    ).toBeNull();
  });

  it("declines when there is nothing upcoming to shuffle", () => {
    expect(
      replanUpcoming({
        mode: "shuffle",
        context: null,
        queueTrackIds: ["a"],
        nowPlayingIndex: 0,
      }),
    ).toBeNull();
  });
});

describe("interleave", () => {
  it("drops one extra after every `every` base tracks", () => {
    expect(interleave(["a", "b", "c", "d", "e", "f"], ["X", "Y"], 3)).toEqual([
      "a", "b", "c", "X", "d", "e", "f", "Y",
    ]);
  });

  it("appends extras that outlast the base list", () => {
    expect(interleave(["a", "b"], ["X", "Y", "Z"], 2)).toEqual(["a", "b", "X", "Y", "Z"]);
  });

  it("returns a copy of the base when there is nothing to mix in", () => {
    const base = ["a", "b"];
    const out = interleave(base, [], 3);
    expect(out).toEqual(base);
    expect(out).not.toBe(base);
  });

  it("guards against a zero or negative step", () => {
    expect(interleave(["a", "b"], ["X"], 0)).toEqual(["a", "X", "b"]);
  });
});

describe("recommendationSlots", () => {
  beforeEach(() => vi.restoreAllMocks());

  it("asks for one per `every` tracks", () => {
    expect(recommendationSlots(12, 4, 10)).toBe(3);
  });

  it("caps at max", () => {
    expect(recommendationSlots(400, 4, 10)).toBe(10);
  });

  it("asks for none when the queue is too short to hide one in", () => {
    expect(recommendationSlots(3, 4, 10)).toBe(0);
    expect(recommendationSlots(0, 4, 10)).toBe(0);
  });
});
