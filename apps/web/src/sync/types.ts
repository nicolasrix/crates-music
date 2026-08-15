// Wire types for the gateway sync protocol. Mirror Rust shapes in
// `music-core` and `music-sync`. Kept narrow: we only model what the
// web client actually reads or writes.

export interface QueueItem {
  item_id: string;
  track_id: string;
}

export interface Queue {
  items: QueueItem[];
}

/// The intent tier of playback state — which user-initiated session
/// owns the current queue. `started_ms` is server-stamped (epoch ms);
/// the client mirror may temporarily hold a `Date.now()` guess for an
/// optimistic StartSession, which is replaced as soon as the next
/// frame from the server reconciles.
export interface SessionAnchor {
  session_id: string;
  track_id: string;
  started_ms: number;
}

export interface PlaybackState {
  queue: Queue;
  now_playing_index: number | null;
  position_ms: number;
  is_playing: boolean;
  // Absent on the wire when there's no active session — we normalize
  // to `null` here so consumers don't have to guard against `undefined`.
  session_anchor: SessionAnchor | null;
}

export interface SyncState {
  playback: PlaybackState;
  version: number;
}

export type SyncOp =
  | { type: "push"; item_id: string; track_id: string }
  | { type: "remove"; item_id: string }
  | { type: "reorder"; item_id: string; new_index: number }
  | { type: "set_now_playing"; index: number | null }
  | { type: "set_position"; position_ms: number }
  | { type: "set_playing"; is_playing: boolean }
  | { type: "clear" }
  | {
      type: "start_session";
      items: QueueItem[];
      anchor_index: number;
      session_id: string;
    }
  // Swap everything after the cursor, leaving the current track (and its
  // position, play state and session) alone. Backs the shuffle modes:
  // reshuffle, restore-original-order, and mixing recommendations into a
  // context are all "rewrite the upcoming half" and nothing else.
  | { type: "replace_upcoming"; items: QueueItem[] }
  | { type: "stop_session" };

export type ServerMessage =
  | { type: "snapshot"; state: SyncState }
  | { type: "applied"; op: SyncOp; version: number }
  | { type: "op_error"; message: string };

export type ClientMessage = { type: "op"; op: SyncOp };
