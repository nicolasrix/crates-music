import { describe, expect, it } from "vitest";
import { enqueuePending, projectState, settlePending, type PendingOp } from "./pending";
import type { SyncOp, SyncState } from "./types";

const confirmed = (): SyncState => ({
  version: 7,
  playback: {
    queue: {
      items: [
        { item_id: "qi-0", track_id: "t-0" },
        { item_id: "qi-1", track_id: "t-1" },
        { item_id: "qi-2", track_id: "t-2" },
      ],
    },
    now_playing_index: 0,
    position_ms: 0,
    is_playing: true,
    session_anchor: null,
  },
});

const p = (opId: string, op: SyncOp): PendingOp => ({ opId, op });
const next = (index: number) => ({ type: "set_now_playing", index }) as const;

describe("projectState", () => {
  it("moves the cursor without waiting for the gateway", () => {
    // The whole bug: a track ends with the phone's network asleep, the
    // advance op can't reach the gateway, and nothing plays. The view
    // must move the moment the op is submitted.
    const view = projectState(confirmed(), [p("a", next(1))]);
    expect(view.playback.now_playing_index).toBe(1);
  });

  it("keeps the confirmed version — optimistic state is not acked state", () => {
    expect(projectState(confirmed(), [p("a", next(1))]).version).toBe(7);
  });

  it("folds pending ops in submission order", () => {
    const view = projectState(confirmed(), [
      p("a", next(2)),
      p("b", { type: "set_playing", is_playing: false }),
    ]);
    expect(view.playback.now_playing_index).toBe(2);
    expect(view.playback.is_playing).toBe(false);
  });

  it("re-applies pending ops over a reconnect snapshot instead of losing them", () => {
    // The snapshot a reconnect delivers predates the ops this client made
    // while offline; adopting it bare would yank playback back a track.
    const staleSnapshot = confirmed();
    const view = projectState(staleSnapshot, [p("a", next(1)), p("b", next(2))]);
    expect(view.playback.now_playing_index).toBe(2);
  });

  it("is the confirmed state itself when nothing is pending", () => {
    const c = confirmed();
    expect(projectState(c, [])).toBe(c);
  });
});

describe("enqueuePending", () => {
  it("collapses back-to-back absolute setters of the same kind", () => {
    // An hour of screen-off listening is a run of set_now_playing ops;
    // replaying every one would step other devices through each track.
    const q = enqueuePending(enqueuePending([], p("a", next(1))), p("b", next(2)));
    expect(q).toEqual([p("b", next(2))]);
  });

  it("does not collapse across an op that reads the superseded field", () => {
    // replace_upcoming keeps everything up to the cursor, so the cursor
    // move before it still matters.
    const replace: SyncOp = { type: "replace_upcoming", items: [] };
    let q: PendingOp[] = [];
    q = enqueuePending(q, p("a", next(1)));
    q = enqueuePending(q, p("b", replace));
    q = enqueuePending(q, p("c", next(2)));
    expect(q.map((e) => e.opId)).toEqual(["a", "b", "c"]);
  });

  it("never collapses ops that are not absolute setters", () => {
    const push = (id: string): SyncOp => ({ type: "push", item_id: id, track_id: id });
    const q = enqueuePending(enqueuePending([], p("a", push("x"))), p("b", push("y")));
    expect(q).toHaveLength(2);
  });

  it("does not mutate its input", () => {
    const q: PendingOp[] = [p("a", next(1))];
    enqueuePending(q, p("b", next(2)));
    expect(q).toEqual([p("a", next(1))]);
  });
});

describe("settlePending", () => {
  it("drops the acked op wherever it sits", () => {
    const q = [p("a", next(1)), p("b", { type: "set_playing", is_playing: false })];
    expect(settlePending(q, "b").map((e) => e.opId)).toEqual(["a"]);
  });

  it("ignores ids it does not hold (another device's op, or a collapsed one)", () => {
    const q = [p("a", next(1))];
    expect(settlePending(q, "zzz")).toBe(q);
    expect(settlePending(q, undefined)).toBe(q);
  });
});
