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
import { useToast } from "../toast/ToastContext";
import { applyOp } from "./apply";
import { reconnectDelayMs } from "./reconnect";
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
   *  one round-trip — replaces the legacy 4-op pattern. Returns the
   *  new session id so the caller can file what it started under
   *  (see PlayModeContext's context memory). */
  startSession: (tracks: readonly Track[], anchorIndex: number) => string;
  /** Swap everything after the cursor for `trackIds`, leaving the
   *  current track playing untouched. `meta` is optional metadata to
   *  stash for ids we already have Tracks for (otherwise the queue
   *  hydration effect fetches them). Returns the minted item ids,
   *  positionally matching `trackIds`. */
  replaceUpcoming: (trackIds: readonly string[], meta?: readonly Track[]) => string[];
  ready: boolean;
}

const Ctx = createContext<SyncCtx | null>(null);

export function SyncProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<SyncState>(EMPTY);
  const [ready, setReady] = useState(false);
  const toast = useToast();
  const wsRef = useRef<WebSocket | null>(null);
  // Ops submitted before the WS reaches OPEN are buffered here and
  // flushed on `onopen`. Without this, a user click that lands during
  // the connect window (or briefly during reconnect) is silently
  // dropped — the previous 4-op pattern hid this by sometimes losing
  // only a subset; start_session makes it a binary "empty queue" miss.
  const outboxRef = useRef<SyncOp[]>([]);
  // Consecutive failed/closed connections, reset on a successful open.
  // Drives the backoff schedule in reconnect.ts.
  const attemptRef = useRef(0);
  const retryTimerRef = useRef<number | null>(null);
  const trackMetaRef = useRef<Map<string, Track>>(new Map());
  // Bumped whenever trackMeta gains entries; included in the context
  // value's memo deps so consumers re-render on metadata arrival even
  // when no sync-state change accompanies it (the hydration path).
  const [metaTick, forceMetaTick] = useState(0);
  const metaInflight = useRef<Set<string>>(new Set());

  const tokens = readTokens();
  const accessToken = tokens?.accessToken;

  // Open the socket and keep it open.
  //
  // Reconnect is load-bearing, not a nicety. A phone changes network far
  // more often than a desktop does (WiFi → cellular, tunnel re-establish,
  // radio sleep), and every one of those closes the socket. Before this,
  // `onclose` only cleared `ready` — so a single handover left the tab
  // wedged on whatever snapshot it happened to hold, forever. Diagnosed
  // live 2026-08-03: a 21-second socket on mobile data, then eleven
  // minutes of a player bar describing a track that had stopped playing
  // on another device hours earlier, and a scrobble credited to it.
  useEffect(() => {
    if (!accessToken) return;
    // Guards every async continuation below. React re-runs this effect on
    // token change (and twice under StrictMode); without it a torn-down
    // effect would schedule reconnects for a socket nobody is reading.
    let disposed = false;

    const clearRetry = () => {
      if (retryTimerRef.current !== null) {
        clearTimeout(retryTimerRef.current);
        retryTimerRef.current = null;
      }
    };

    const scheduleReconnect = () => {
      // A pending timer already owns the next attempt — `onclose` and an
      // `offline`/`online` flap can otherwise both try to schedule one.
      if (disposed || retryTimerRef.current !== null) return;
      const delay = reconnectDelayMs(attemptRef.current);
      attemptRef.current += 1;
      retryTimerRef.current = window.setTimeout(() => {
        retryTimerRef.current = null;
        connect();
      }, delay);
    };

    const connect = () => {
      if (disposed) return;
      // Connect via the Vite dev proxy (ws: true). Production same-origin
      // means the gateway sees the upgrade directly.
      const wsScheme = location.protocol === "https:" ? "wss" : "ws";
      const url = `${wsScheme}://${location.host}/v1/sync?access_token=${encodeURIComponent(accessToken)}`;
      const ws = new WebSocket(url);
      wsRef.current = ws;
      ws.onopen = () => {
        attemptRef.current = 0;
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
          // A rejected op means an optimistic UI change silently reverted —
          // tell the user why instead of leaving a mystery rollback.
          console.warn("[sync] op rejected:", msg.message);
          toast(`sync: ${msg.message}`, { variant: "error" });
        }
      };
      // `onerror` is always followed by `onclose`, so retry is driven from
      // one place only. Note a handshake rejected for an expired token
      // also lands here: the backoff caps that at one probe per 30 s until
      // AuthContext refreshes and re-runs this effect with a new token.
      ws.onclose = () => {
        if (disposed) return;
        setReady(false);
        scheduleReconnect();
      };
    };

    // Coming back online, or the tab returning to the foreground on a
    // phone, is a far better retry trigger than waiting out the backoff —
    // reconnect immediately instead of up to 30 s later.
    const reconnectNow = () => {
      const ws = wsRef.current;
      if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;
      clearRetry();
      attemptRef.current = 0;
      connect();
    };
    const onVisibility = () => {
      if (document.visibilityState === "visible") reconnectNow();
    };
    window.addEventListener("online", reconnectNow);
    document.addEventListener("visibilitychange", onVisibility);

    connect();

    return () => {
      disposed = true;
      clearRetry();
      window.removeEventListener("online", reconnectNow);
      document.removeEventListener("visibilitychange", onVisibility);
      wsRef.current?.close();
      wsRef.current = null;
    };
    // `toast` is referentially stable (useCallback in ToastProvider), so
    // listing it doesn't churn the socket.
  }, [accessToken, toast]);

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
      if (tracks.length === 0) return "";
      for (const t of tracks) trackMetaRef.current.set(t.id, t);
      forceMetaTick((n) => n + 1);
      const items = tracks.map((t) => ({ item_id: newItemId(), track_id: t.id }));
      const sessionId = newSessionId();
      submit({
        type: "start_session",
        items,
        anchor_index: anchorIndex,
        session_id: sessionId,
      });
      return sessionId;
    },
    [submit],
  );

  const replaceUpcoming = useCallback(
    (trackIds: readonly string[], meta?: readonly Track[]) => {
      if (meta && meta.length > 0) {
        for (const t of meta) trackMetaRef.current.set(t.id, t);
        forceMetaTick((n) => n + 1);
      }
      const items = trackIds.map((id) => ({ item_id: newItemId(), track_id: id }));
      submit({ type: "replace_upcoming", items });
      return items.map((i) => i.item_id);
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
      replaceUpcoming,
      ready,
    }),
    // metaTick: trackMeta is a mutable ref; the tick is its change signal.
    [state, ready, submit, pushTrack, startSession, replaceUpcoming, metaTick],
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
