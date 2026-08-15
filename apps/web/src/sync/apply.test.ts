// Mirror of the server's state-machine semantics for `start_session`
// and `stop_session`. The full mechanics suite (push/remove/reorder
// cursor follow, etc.) lives on the Rust side — these tests are
// specifically for the variants the web reducer has to handle.

import { describe, expect, it } from "vitest";
import { applyOp } from "./apply";
import type { SyncOp, SyncState } from "./types";

const emptyState = (): SyncState => ({
  version: 0,
  playback: {
    queue: { items: [] },
    now_playing_index: null,
    position_ms: 0,
    is_playing: false,
    session_anchor: null,
  },
});

const startSession = (opts?: {
  items?: { item_id: string; track_id: string }[];
  anchor_index?: number;
  session_id?: string;
}): SyncOp => ({
  type: "start_session",
  items: opts?.items ?? [{ item_id: "qi-1", track_id: "t-1" }],
  anchor_index: opts?.anchor_index ?? 0,
  session_id: opts?.session_id ?? "sess-1",
});

describe("applyOp — start_session", () => {
  it("replaces queue, sets cursor at anchor_index, sets is_playing=true", () => {
    const before = emptyState();
    // Pre-existing queue + cursor must be discarded.
    before.playback.queue.items = [{ item_id: "old", track_id: "t-old" }];
    before.playback.now_playing_index = 0;
    before.playback.position_ms = 12_345;

    const after = applyOp(
      before,
      startSession({
        items: [
          { item_id: "qi-1", track_id: "t-1" },
          { item_id: "qi-2", track_id: "t-2" },
        ],
        anchor_index: 1,
        session_id: "sess-x",
      }),
      1,
    );
    expect(after.playback.queue.items.map((i) => i.item_id)).toEqual(["qi-1", "qi-2"]);
    expect(after.playback.now_playing_index).toBe(1);
    expect(after.playback.is_playing).toBe(true);
    expect(after.playback.position_ms).toBe(0);
  });

  it("sets session_anchor with anchor track and the op's session_id", () => {
    const after = applyOp(
      emptyState(),
      startSession({
        items: [
          { item_id: "qi-1", track_id: "t-1" },
          { item_id: "qi-2", track_id: "t-2" },
        ],
        anchor_index: 1,
        session_id: "sess-x",
      }),
      1,
    );
    expect(after.playback.session_anchor).toBeDefined();
    expect(after.playback.session_anchor?.session_id).toBe("sess-x");
    expect(after.playback.session_anchor?.track_id).toBe("t-2");
    // started_ms is filled by the server; the client mirror leaves it
    // as whatever the server-stamped value is — represented as null
    // until the next snapshot/applied frame reconciles it.
    expect(typeof after.playback.session_anchor?.started_ms).toBe("number");
  });

  it("returns a new SyncState object (immutability)", () => {
    const before = emptyState();
    const after = applyOp(before, startSession(), 1);
    expect(after).not.toBe(before);
    expect(after.playback).not.toBe(before.playback);
    expect(before.playback.queue.items).toEqual([]);
  });

  it("bumps version to newVersion", () => {
    const after = applyOp(emptyState(), startSession(), 42);
    expect(after.version).toBe(42);
  });
});

describe("applyOp — stop_session", () => {
  it("nulls session_anchor without touching queue, cursor, or play flag", () => {
    let s = applyOp(emptyState(), startSession(), 1);
    expect(s.playback.session_anchor).toBeDefined();
    const queueBefore = s.playback.queue.items;
    const cursorBefore = s.playback.now_playing_index;
    const playingBefore = s.playback.is_playing;

    s = applyOp(s, { type: "stop_session" }, 2);
    expect(s.playback.session_anchor).toBeNull();
    expect(s.playback.queue.items).toEqual(queueBefore);
    expect(s.playback.now_playing_index).toBe(cursorBefore);
    expect(s.playback.is_playing).toBe(playingBefore);
  });

  it("is a no-op (but bumps version) when no session is active", () => {
    const after = applyOp(emptyState(), { type: "stop_session" }, 1);
    expect(after.version).toBe(1);
    expect(after.playback.session_anchor).toBeNull();
  });
});

describe("applyOp — clear with anchor", () => {
  it("clear nulls the session anchor along with the rest", () => {
    let s = applyOp(emptyState(), startSession(), 1);
    expect(s.playback.session_anchor).toBeDefined();
    s = applyOp(s, { type: "clear" }, 2);
    expect(s.playback.session_anchor).toBeNull();
    expect(s.playback.queue.items).toEqual([]);
    expect(s.playback.now_playing_index).toBeNull();
  });
});

describe("applyOp — mechanics ops preserve session_anchor", () => {
  it("push does not change the anchor", () => {
    let s = applyOp(emptyState(), startSession(), 1);
    const anchor = s.playback.session_anchor;
    s = applyOp(s, { type: "push", item_id: "qi-9", track_id: "t-9" }, 2);
    expect(s.playback.session_anchor).toEqual(anchor);
  });

  it("set_now_playing does not change the anchor", () => {
    let s = applyOp(
      emptyState(),
      startSession({
        items: [
          { item_id: "qi-1", track_id: "t-1" },
          { item_id: "qi-2", track_id: "t-2" },
        ],
      }),
      1,
    );
    const anchor = s.playback.session_anchor;
    s = applyOp(s, { type: "set_now_playing", index: 1 }, 2);
    expect(s.playback.session_anchor).toEqual(anchor);
  });
});

describe("applyOp — replace_upcoming", () => {
  // The local mirror must agree with `SyncState::apply_replace_upcoming`
  // exactly: a shuffle flip that diverges here shows a different queue on
  // this device than every other one until the next snapshot.
  const withQueue = (items: string[], cursor: number | null): SyncState => {
    const s = emptyState();
    s.playback.queue.items = items.map((id) => ({ item_id: id, track_id: `t-${id}` }));
    s.playback.now_playing_index = cursor;
    s.playback.position_ms = 42_000;
    s.playback.is_playing = true;
    s.playback.session_anchor = { session_id: "sess-1", track_id: "t-a", started_ms: 1 };
    return s;
  };
  const replace = (ids: string[]): SyncOp => ({
    type: "replace_upcoming",
    items: ids.map((id) => ({ item_id: id, track_id: `t-${id}` })),
  });

  it("keeps history and the current track, swaps the tail", () => {
    const after = applyOp(withQueue(["a", "b", "c", "d"], 1), replace(["x", "y"]), 7);
    expect(after.playback.queue.items.map((i) => i.item_id)).toEqual(["a", "b", "x", "y"]);
    expect(after.playback.now_playing_index).toBe(1);
    expect(after.version).toBe(7);
  });

  it("leaves position, play state and session anchor alone", () => {
    const before = withQueue(["a", "b"], 0);
    const after = applyOp(before, replace(["z"]), 2);
    expect(after.playback.position_ms).toBe(42_000);
    expect(after.playback.is_playing).toBe(true);
    expect(after.playback.session_anchor).toEqual(before.playback.session_anchor);
  });

  it("replaces the whole queue when there is no cursor", () => {
    const after = applyOp(withQueue(["a", "b"], null), replace(["z"]), 2);
    expect(after.playback.queue.items.map((i) => i.item_id)).toEqual(["z"]);
    expect(after.playback.now_playing_index).toBeNull();
  });

  it("drops ids that would duplicate the kept prefix or each other", () => {
    const after = applyOp(withQueue(["a", "b"], 0), replace(["n", "a", "n"]), 2);
    expect(after.playback.queue.items.map((i) => i.item_id)).toEqual(["a", "n"]);
  });

  it("truncates to the current track on an empty item list", () => {
    const after = applyOp(withQueue(["a", "b", "c"], 0), replace([]), 2);
    expect(after.playback.queue.items.map((i) => i.item_id)).toEqual(["a"]);
  });
});
