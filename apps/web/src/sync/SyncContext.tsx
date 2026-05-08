// SyncContext: maintains the gateway-mirrored playback state, opens a
// WebSocket on mount, and exposes a single `submit(op)` so callers
// don't deal with the wire format. Track metadata for queue items
// lives here too — server-stored queue is just (item_id, track_id);
// we cache full Track objects locally as we push.

import {
  createContext,
  ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { Track } from "../api/types";
import { readTokens } from "../auth/tokens";
import { applyOp } from "./apply";
import type { ClientMessage, ServerMessage, SyncOp, SyncState } from "./types";

const EMPTY: SyncState = {
  playback: { queue: { items: [] }, now_playing_index: null, position_ms: 0, is_playing: false },
  version: 0,
};

interface SyncCtx {
  state: SyncState;
  /** Best-effort metadata for the items currently in the queue. */
  trackMeta: Map<string, Track>;
  /** Submit an op to the gateway. Local state will update when the
   *  matching `applied` frame arrives. */
  submit: (op: SyncOp) => void;
  /** Push a track and stash its metadata locally so the player bar can
   *  render it without a separate fetch. */
  pushTrack: (track: Track) => string;
  ready: boolean;
}

const Ctx = createContext<SyncCtx | null>(null);

export function SyncProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<SyncState>(EMPTY);
  const [ready, setReady] = useState(false);
  const wsRef = useRef<WebSocket | null>(null);
  const trackMetaRef = useRef<Map<string, Track>>(new Map());
  const [, forceMetaTick] = useState(0);

  const tokens = readTokens();
  const accessToken = tokens?.accessToken;

  useEffect(() => {
    if (!accessToken) return;
    // Connect via the Vite dev proxy (ws: true). Production same-origin
    // means the gateway sees the upgrade directly.
    const wsScheme = location.protocol === "https:" ? "wss" : "ws";
    const url = `${wsScheme}://${location.host}/v1/sync?access_token=${encodeURIComponent(accessToken)}`;
    const ws = new WebSocket(url);
    wsRef.current = ws;
    ws.onmessage = (ev) => {
      let msg: ServerMessage;
      try {
        msg = JSON.parse(ev.data) as ServerMessage;
      } catch {
        return;
      }
      if (msg.type === "snapshot") {
        setState(msg.state);
        setReady(true);
      } else if (msg.type === "applied") {
        setState((s) => applyOp(s, msg.op, msg.version));
      }
      // op_error is informational — the local state will not advance,
      // but we don't have user-facing toasts yet, so just log.
      else if (msg.type === "op_error") {
        console.warn("sync op rejected:", msg.message);
      }
    };
    ws.onclose = () => {
      setReady(false);
    };
    return () => {
      ws.close();
      wsRef.current = null;
    };
  }, [accessToken]);

  const submit = useCallback((op: SyncOp) => {
    const ws = wsRef.current;
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    const msg: ClientMessage = { type: "op", op };
    ws.send(JSON.stringify(msg));
  }, []);

  const pushTrack = useCallback(
    (track: Track) => {
      const itemId = newItemId();
      trackMetaRef.current.set(track.id, track);
      forceMetaTick((n) => n + 1);
      submit({ type: "push", item_id: itemId, track_id: track.id });
      return itemId;
    },
    [submit],
  );

  const value = useMemo<SyncCtx>(
    () => ({
      state,
      trackMeta: trackMetaRef.current,
      submit,
      pushTrack,
      ready,
    }),
    [state, ready, submit, pushTrack],
  );

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useSync(): SyncCtx {
  const v = useContext(Ctx);
  if (!v) throw new Error("useSync must be used inside <SyncProvider>");
  return v;
}

// ULID-ish: timestamp prefix + random suffix. Plenty unique for a
// single-user app and stays short enough to read in dev tools.
function newItemId(): string {
  const ts = Date.now().toString(36);
  const rand = Math.random().toString(36).slice(2, 10);
  return `${ts}-${rand}`;
}
