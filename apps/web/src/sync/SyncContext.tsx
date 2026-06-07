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
import { getSong } from "../api/client";
import { Track } from "../api/types";
import { readTokens } from "../auth/tokens";
import { applyOp } from "./apply";
import type { ClientMessage, ServerMessage, SyncOp, SyncState } from "./types";

const EMPTY: SyncState = {
  playback: {
    queue: { items: [] },
    now_playing_index: null,
    position_ms: 0,
    is_playing: false,
    session_anchor: null,
  },
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
  /** Atomically replace the queue with `tracks` and start a fresh
   *  recommend-session anchored on `tracks[anchorIndex]`. One op,
   *  one round-trip — replaces the legacy 4-op pattern. */
  startSession: (tracks: readonly Track[], anchorIndex: number) => void;
  ready: boolean;
}

const Ctx = createContext<SyncCtx | null>(null);

export function SyncProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<SyncState>(EMPTY);
  const [ready, setReady] = useState(false);
  const wsRef = useRef<WebSocket | null>(null);
  // Ops submitted before the WS reaches OPEN are buffered here and
  // flushed on `onopen`. Without this, a user click that lands during
  // the connect window (or briefly during reconnect) is silently
  // dropped — the previous 4-op pattern hid this by sometimes losing
  // only a subset; start_session makes it a binary "empty queue" miss.
  const outboxRef = useRef<SyncOp[]>([]);
  const trackMetaRef = useRef<Map<string, Track>>(new Map());
  // Bumped whenever trackMeta gains entries; included in the context
  // value's memo deps so consumers re-render on metadata arrival even
  // when no sync-state change accompanies it (the hydration path).
  const [metaTick, forceMetaTick] = useState(0);
  const metaInflight = useRef<Set<string>>(new Set());

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
    ws.onopen = () => {
      console.log("[sync] ws open, flushing outbox:", outboxRef.current.length);
      const pending = outboxRef.current;
      outboxRef.current = [];
      for (const op of pending) {
        const msg: ClientMessage = { type: "op", op };
        ws.send(JSON.stringify(msg));
      }
    };
    ws.onmessage = (ev) => {
      let msg: ServerMessage;
      try {
        msg = JSON.parse(ev.data) as ServerMessage;
      } catch {
        console.warn("[sync] failed to parse ws frame:", ev.data);
        return;
      }
      if (msg.type === "snapshot") {
        console.log("[sync] snapshot v=" + msg.state.version, "items=" + msg.state.playback.queue.items.length);
        setState(msg.state);
        setReady(true);
      } else if (msg.type === "applied") {
        console.log("[sync] applied v=" + msg.version, "op=" + msg.op.type);
        setState((s) => applyOp(s, msg.op, msg.version));
      } else if (msg.type === "op_error") {
        console.warn("[sync] op rejected:", msg.message);
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

  // Hydrate metadata for queue items we didn't push ourselves. trackMeta
  // is in-memory only, so after a page reload (the *normal* lifecycle for
  // an installed PWA) a snapshot-restored queue has ids but no titles —
  // which blanks the player bar, Media Session, and the queue rows' menus.
  // Backfill via getSong, deduped against in-flight fetches. A failed
  // fetch is retried only on the next queue change (no hot loop offline).
  useEffect(() => {
    const missing = [
      ...new Set(state.playback.queue.items.map((it) => it.track_id)),
    ].filter(
      (id) => !trackMetaRef.current.has(id) && !metaInflight.current.has(id),
    );
    if (missing.length === 0) return;
    for (const id of missing) metaInflight.current.add(id);
    void Promise.all(
      missing.map(async (id) => {
        try {
          const t = await getSong(id);
          trackMetaRef.current.set(id, t);
          return true;
        } catch {
          metaInflight.current.delete(id);
          return false;
        }
      }),
    ).then((results) => {
      if (results.some(Boolean)) forceMetaTick((n) => n + 1);
    });
  }, [state.playback.queue.items]);

  const submit = useCallback((op: SyncOp) => {
    const ws = wsRef.current;
    // If the socket isn't open yet (race during connect/reconnect),
    // buffer the op. `onopen` drains the outbox in submission order.
    if (!ws || ws.readyState !== WebSocket.OPEN) {
      outboxRef.current.push(op);
      return;
    }
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

  const startSession = useCallback(
    (tracks: readonly Track[], anchorIndex: number) => {
      if (tracks.length === 0) return;
      for (const t of tracks) trackMetaRef.current.set(t.id, t);
      forceMetaTick((n) => n + 1);
      const items = tracks.map((t) => ({ item_id: newItemId(), track_id: t.id }));
      submit({
        type: "start_session",
        items,
        anchor_index: anchorIndex,
        session_id: newSessionId(),
      });
    },
    [submit],
  );

  const value = useMemo<SyncCtx>(
    () => ({
      state,
      trackMeta: trackMetaRef.current,
      submit,
      pushTrack,
      startSession,
      ready,
    }),
    // metaTick: trackMeta is a mutable ref; the tick is its change signal.
    [state, ready, submit, pushTrack, startSession, metaTick],
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

// Session ids: prefer crypto.randomUUID() (always available in HTTPS
// contexts, which we are by gateway constraint), fall back to the
// same ULID-ish shape so tests in non-secure contexts still work.
function newSessionId(): string {
  const c = globalThis.crypto;
  if (c && typeof c.randomUUID === "function") return c.randomUUID();
  return `sess-${newItemId()}`;
}
