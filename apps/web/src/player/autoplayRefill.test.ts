import { describe, expect, it } from "vitest";
import {
  isUnderdelivery,
  needsRearm,
  planRefill,
  type RefillPlanInput,
} from "./autoplayRefill";
import type { Track } from "../api/types";
import type { QueueItem } from "../sync/types";

const item = (track_id: string): QueueItem => ({
  item_id: `i-${track_id}`,
  track_id,
});
const track = (id: string): Track => ({ id, title: `title-${id}` });

/** A refill that asked for 5, against a one-item queue in session
 *  `sess-1`, and got 5 back. Individual tests override the live view. */
const base = (over: Partial<RefillPlanInput> = {}): RefillPlanInput => ({
  requestSessionId: "sess-1",
  liveItems: [item("seed")],
  liveNowPlayingIndex: 0,
  liveSessionId: "sess-1",
  tracks: [track("a"), track("b"), track("c"), track("d"), track("e")],
  minUpcoming: 5,
  ...over,
});

describe("planRefill", () => {
  it("pushes the whole result set when the queue still needs it", () => {
    const plan = planRefill(base());
    expect(plan.push.map((t) => t.id)).toEqual(["a", "b", "c", "d", "e"]);
    expect(plan.target).toBe(5);
    expect(plan.disposition).toBe("delivered");
  });

  // The regression this module exists for. The queue moving under an
  // in-flight refill is the normal case, not an error: the `applied`
  // frame for the click that triggered the refill is itself a queue
  // change. The old code discarded the entire result set here, and
  // nothing retried — autoplay went silent with a one-item queue while
  // the gateway was happily serving recommendations.
  it("still pushes when the queue moved but the session did not", () => {
    const plan = planRefill(
      base({ liveItems: [item("seed"), item("other")], liveNowPlayingIndex: 1 }),
    );
    expect(plan.push.map((t) => t.id)).toEqual(["a", "b", "c", "d", "e"]);
    expect(plan.disposition).toBe("delivered");
  });

  it("discards the result set when a different session started", () => {
    const plan = planRefill(base({ liveSessionId: "sess-2" }));
    expect(plan.push).toEqual([]);
    expect(plan.disposition).toBe("stale");
  });

  it("treats a session-less queue as valid while it stays session-less", () => {
    const plan = planRefill(
      base({ requestSessionId: undefined, liveSessionId: undefined }),
    );
    expect(plan.push).toHaveLength(5);
    expect(plan.disposition).toBe("delivered");
  });

  it("discards the result set when the cursor is gone", () => {
    const plan = planRefill(base({ liveNowPlayingIndex: null }));
    expect(plan.push).toEqual([]);
    expect(plan.disposition).toBe("stale");
  });

  it("pushes nothing when the queue filled up while we were fetching", () => {
    const plan = planRefill(
      base({
        liveItems: [
          item("seed"),
          item("u1"),
          item("u2"),
          item("u3"),
          item("u4"),
          item("u5"),
        ],
      }),
    );
    expect(plan.push).toEqual([]);
    expect(plan.disposition).toBe("satisfied");
  });

  // Recomputing the target is what stops two overlapping refills from
  // stacking their full result sets on top of each other.
  it("recomputes how many are needed against the live queue", () => {
    const plan = planRefill(
      base({ liveItems: [item("seed"), item("u1"), item("u2"), item("u3")] }),
    );
    expect(plan.target).toBe(2);
    expect(plan.push.map((t) => t.id)).toEqual(["a", "b"]);
    expect(plan.disposition).toBe("delivered");
  });

  it("skips tracks already in the live queue", () => {
    const plan = planRefill(base({ liveItems: [item("seed"), item("b")] }));
    expect(plan.push.map((t) => t.id)).toEqual(["a", "c", "d", "e"]);
  });

  it("skips duplicates within one result set", () => {
    const plan = planRefill({
      ...base(),
      tracks: [track("a"), track("a"), track("b")],
    });
    expect(plan.push.map((t) => t.id)).toEqual(["a", "b"]);
    expect(plan.disposition).toBe("short");
  });

  it("reports short when fewer usable tracks come back than needed", () => {
    const plan = planRefill({ ...base(), tracks: [track("a")] });
    expect(plan.push.map((t) => t.id)).toEqual(["a"]);
    expect(plan.target).toBe(5);
    expect(plan.disposition).toBe("short");
  });
});

describe("needsRearm", () => {
  // A partial push does change `queue.items`, but the in-flight lock is
  // still held when that re-triggers the effect, so the natural wake-up
  // is swallowed. Short must re-arm even though it placed something.
  it("re-arms on the two dispositions that leave the queue short", () => {
    expect(needsRearm(planRefill(base({ liveSessionId: "sess-2" })))).toBe(true);
    expect(needsRearm(planRefill({ ...base(), tracks: [track("a")] }))).toBe(
      true,
    );
  });

  it("does not re-arm when the queue reached threshold", () => {
    expect(needsRearm(planRefill(base()))).toBe(false);
    expect(
      needsRearm(
        planRefill(
          base({
            liveItems: [
              item("seed"),
              item("u1"),
              item("u2"),
              item("u3"),
              item("u4"),
              item("u5"),
            ],
          }),
        ),
      ),
    ).toBe(false);
  });
});

describe("isUnderdelivery", () => {
  it("is true only for a genuinely short result set", () => {
    expect(isUnderdelivery(planRefill({ ...base(), tracks: [track("a")] }))).toBe(
      true,
    );
  });

  // A stale result is not the recommender's fault, so it must not eat the
  // 30s back-off — the point of re-arming is to re-ask promptly for the
  // session that superseded it.
  it("is false for a stale result", () => {
    expect(isUnderdelivery(planRefill(base({ liveSessionId: "sess-2" })))).toBe(
      false,
    );
  });

  it("is false when the refill delivered", () => {
    expect(isUnderdelivery(planRefill(base()))).toBe(false);
  });
});
