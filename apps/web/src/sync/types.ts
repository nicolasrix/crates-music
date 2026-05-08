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

export interface PlaybackState {
  queue: Queue;
  now_playing_index: number | null;
  position_ms: number;
  is_playing: boolean;
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
  | { type: "clear" };

export type ServerMessage =
  | { type: "snapshot"; state: SyncState }
  | { type: "applied"; op: SyncOp; version: number }
  | { type: "op_error"; message: string };

export type ClientMessage = { type: "op"; op: SyncOp };
