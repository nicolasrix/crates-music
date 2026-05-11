// Client-side mirror of music-sync's apply logic. Keeps the local
// SyncState convergent with the server: applying an op locally must
// produce the same result as the server applying it. Used to fold
// `applied` frames into local state.

import type { PlaybackState, SyncOp, SyncState } from "./types";

export function applyOp(state: SyncState, op: SyncOp, newVersion: number): SyncState {
  return {
    version: newVersion,
    playback: applyToPlayback(state.playback, op),
  };
}

function applyToPlayback(p: PlaybackState, op: SyncOp): PlaybackState {
  switch (op.type) {
    case "push": {
      if (p.queue.items.some((i) => i.item_id === op.item_id)) return p;
      return {
        ...p,
        queue: { items: [...p.queue.items, { item_id: op.item_id, track_id: op.track_id }] },
      };
    }
    case "remove": {
      const idx = p.queue.items.findIndex((i) => i.item_id === op.item_id);
      if (idx < 0) return p;
      const items = p.queue.items.filter((_, i) => i !== idx);
      let cursor = p.now_playing_index;
      if (cursor !== null) {
        if (idx < cursor) cursor = cursor - 1;
        if (cursor >= items.length) cursor = null;
      }
      return { ...p, queue: { items }, now_playing_index: cursor };
    }
    case "reorder": {
      const old = p.queue.items.findIndex((i) => i.item_id === op.item_id);
      if (old < 0) return p;
      const cursorItemId =
        p.now_playing_index !== null ? p.queue.items[p.now_playing_index]?.item_id : undefined;
      const next = [...p.queue.items];
      const [moved] = next.splice(old, 1);
      const target = Math.min(op.new_index, next.length);
      next.splice(target, 0, moved!);
      const cursor =
        cursorItemId !== undefined
          ? next.findIndex((i) => i.item_id === cursorItemId)
          : p.now_playing_index;
      return { ...p, queue: { items: next }, now_playing_index: cursor === -1 ? null : cursor };
    }
    case "set_now_playing":
      return { ...p, now_playing_index: op.index };
    case "set_position":
      return { ...p, position_ms: op.position_ms };
    case "set_playing":
      return { ...p, is_playing: op.is_playing };
    case "clear":
      return {
        queue: { items: [] },
        now_playing_index: null,
        position_ms: 0,
        is_playing: false,
        session_anchor: null,
      };
    case "start_session": {
      // Optimistic local stamp — the next frame from the server
      // overwrites `playback` wholesale with the authoritative anchor
      // (correct `started_ms`), so this guess only lives until the
      // ack round-trip.
      const anchor_track = op.items[op.anchor_index]?.track_id;
      if (anchor_track === undefined) return p;
      return {
        queue: { items: op.items.map((i) => ({ item_id: i.item_id, track_id: i.track_id })) },
        now_playing_index: op.anchor_index,
        position_ms: 0,
        is_playing: true,
        session_anchor: {
          session_id: op.session_id,
          track_id: anchor_track,
          started_ms: Date.now(),
        },
      };
    }
    case "stop_session":
      return { ...p, session_anchor: null };
  }
}
