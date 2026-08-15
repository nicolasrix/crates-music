// playSingle / playList must emit a single `start_session` op now,
// not the legacy 4-op pattern (clear + push×N + set_now_playing +
// set_playing). That earlier sequence let observers briefly see a
// queue cleared with no session intent, which is the exact thing the
// atomic StartSession op fixes.

import { describe, expect, it, vi } from "vitest";
import type { Track } from "../api/types";
import { playList, playSingle } from "./playbackHelpers";
import type { SyncOp } from "./types";

function mockSync() {
  const submitted: SyncOp[] = [];
  const startSession = vi.fn((tracks: readonly Track[], anchorIndex: number) => {
    // Mirror the real context: generate one op and record it.
    const op: SyncOp = {
      type: "start_session",
      items: tracks.map((t, i) => ({ item_id: `qi-${i}`, track_id: t.id })),
      anchor_index: anchorIndex,
      session_id: "sess-test",
    };
    submitted.push(op);
    return "sess-test";
  });
  return { startSession, submitted };
}

function track(id: string): Track {
  return {
    id,
    title: `Title ${id}`,
    duration_seconds: 180,
  } as Track;
}

describe("playSingle", () => {
  it("emits one start_session op, not the 4-op pattern", () => {
    const sync = mockSync();
    playSingle(sync, track("t-1"));

    expect(sync.startSession).toHaveBeenCalledOnce();
    expect(sync.submitted).toHaveLength(1);
    expect(sync.submitted[0]!.type).toBe("start_session");
  });

  it("anchor_index is 0 for a single track", () => {
    const sync = mockSync();
    playSingle(sync, track("t-1"));
    const op = sync.submitted[0]! as Extract<SyncOp, { type: "start_session" }>;
    expect(op.anchor_index).toBe(0);
    expect(op.items).toHaveLength(1);
    expect(op.items[0]!.track_id).toBe("t-1");
  });
});

describe("playList", () => {
  it("emits one start_session op carrying every track", () => {
    const sync = mockSync();
    const tracks = [track("t-1"), track("t-2"), track("t-3")];
    playList(sync, tracks, 0);

    expect(sync.startSession).toHaveBeenCalledOnce();
    expect(sync.submitted).toHaveLength(1);
    const op = sync.submitted[0]! as Extract<SyncOp, { type: "start_session" }>;
    expect(op.type).toBe("start_session");
    expect(op.items.map((i) => i.track_id)).toEqual(["t-1", "t-2", "t-3"]);
  });

  it("anchors at startIndex 5, not 0, when user picks a mid-list track", () => {
    const sync = mockSync();
    const tracks = Array.from({ length: 8 }, (_, i) => track(`t-${i}`));
    playList(sync, tracks, 5);
    const op = sync.submitted[0]! as Extract<SyncOp, { type: "start_session" }>;
    expect(op.anchor_index).toBe(5);
  });

  it("empty tracks array is a no-op (no session started)", () => {
    const sync = mockSync();
    playList(sync, [], 0);
    expect(sync.startSession).not.toHaveBeenCalled();
    expect(sync.submitted).toHaveLength(0);
  });
});
