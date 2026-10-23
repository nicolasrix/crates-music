// SyncContext: maintains the gateway-mirrored playback state, opens a
// WebSocket on mount, and exposes a single `submit(op)` so callers
// don't deal with the wire format. Track metadata for queue items
// lives here too — server-stored queue is just (item_id, track_id);
// we cache full Track objects locally as we push.
//
// Ops take effect locally the moment they are submitted. The rendered
// state is the gateway's last confirmed state with this client's
// unacknowledged ops folded over it (pending.ts), so playback never
// waits on the network: a track that ends while the phone's radio is
// asleep still advances. The gateway stays the single linearizer — its
// echoes move the confirmed state, and each echo retires its pending op.

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
import { HEARTBEAT_INTERVAL_MS, PROBE_TIMEOUT_MS, pingOverdue } from "./heartbeat";
import { enqueuePending, projectState, settlePending, type PendingOp } from "./pending";
import { reconnectDelayMs } from "./reconnect";
import { sourcedItems } from "./queueSources";
import type { ClientMessage, PlaySource, ServerMessage, SyncOp, SyncState } from "./types";

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
  /** Submit an op. Local state updates immediately; the op is sent now
   *  if the socket is up, else on the next connect, until acknowledged. */
  submit: (op: SyncOp) => void;
  /** Push a track and stash its metadata locally so the player bar can
   *  render it without a separate fetch. */
  pushTrack: (track: Track, source?: PlaySource) => string;
  /** Atomically replace the queue with `tracks` and start a fresh
   *  recommend-session anchored on `tracks[anchorIndex]`. One op,
   *  one round-trip — replaces the legacy 4-op pattern. Returns the
   *  new session id so the caller can file what it started under
   *  (see PlayModeContext's context memory). */
  startSession: (
    tracks: readonly Track[],
    anchorIndex: number,
    source?: PlaySource,
  ) => string;
  /** Swap everything after the cursor for `trackIds`, leaving the
   *  current track playing untouched. `meta` is optional metadata to
   *  stash for ids we already have Tracks for (otherwise the queue
   *  hydration effect fetches them). Returns the minted item ids,
   *  positionally matching `trackIds`. Each item keeps the source its
   *  track already had in the queue unless `sourceFor` names one. */
  replaceUpcoming: (
    trackIds: readonly string[],
    meta?: readonly Track[],
    sourceFor?: (trackId: string) => PlaySource | undefined,
  ) => string[];
  ready: boolean;
}

const Ctx = createContext<SyncCtx | null>(null);

export function SyncProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<SyncState>(EMPTY);
  const [ready, setReady] = useState(false);
  const toast = useToast();
  const wsRef = useRef<WebSocket | null>(null);
  // Last state the gateway confirmed (snapshot + applied echoes).
  const confirmedRef = useRef<SyncState>(EMPTY);
  // Ops submitted but not yet acknowledged, in submission order. Every
  // one is (re)sent on each `onopen`: an op sent into a socket that was
  // already dead is lost without an error, so "sent" proves nothing
  // until the echo arrives — the gateway de-duplicates by op id. This
  // also covers a click landing before the first connect completes.
  // The provider unmounts on sign-out, so ops never replay into another
  // user's room; a token refresh keeps them, which is the point.
  const pendingRef = useRef<readonly PendingOp[]>([]);
  // When the oldest unanswered ping was sent, or null if none is out.
  const awaitingPongRef = useRef<number | null>(null);
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

  // Re-render from confirmed state + pending ops. Called after either
  // one changes; never mutates them.
  const publish = useCallback(() => {
    setState(projectState(confirmedRef.current, pendingRef.current));
  }, []);

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

    // Write off `ws` without waiting for the browser to agree it is dead:
    // `close()` on a socket whose network vanished can take minutes to
    // reach `onclose`. Detach first so that late event can't schedule a
    // second reconnect.
    const abandon = (ws: WebSocket) => {
      ws.onopen = ws.onmessage = ws.onclose = ws.onerror = null;
      try {
        ws.close();
      } catch {
        /* already closing */
      }
      if (wsRef.current !== ws) return;
      wsRef.current = null;
      awaitingPongRef.current = null;
      setReady(false);
      scheduleReconnect();
    };

    const sendPing = (ws: WebSocket) => {
      const msg: ClientMessage = { type: "ping" };
      ws.send(JSON.stringify(msg));
      awaitingPongRef.current ??= Date.now();
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
        awaitingPongRef.current = null;
        console.log("[sync] ws open, sending pending ops:", pendingRef.current.length);
        for (const p of pendingRef.current) sendOp(ws, p);
      };
      ws.onmessage = (ev) => {
        // Any frame proves the socket is alive.
        awaitingPongRef.current = null;
        let msg: ServerMessage;
        try {
          msg = JSON.parse(ev.data) as ServerMessage;
        } catch {
          console.warn("[sync] failed to parse ws frame:", ev.data);
          return;
        }
        if (msg.type === "snapshot") {
          console.log("[sync] snapshot v=" + msg.state.version, "items=" + msg.state.playback.queue.items.length);
          // Pending ops stay layered on top: this snapshot predates them.
          confirmedRef.current = msg.state;
          publish();
          setReady(true);
        } else if (msg.type === "applied") {
          console.log("[sync] applied v=" + msg.version, "op=" + msg.op.type);
          confirmedRef.current = applyOp(confirmedRef.current, msg.op, msg.version);
          pendingRef.current = settlePending(pendingRef.current, msg.op_id);
          publish();
        } else if (msg.type === "ack") {
          pendingRef.current = settlePending(pendingRef.current, msg.op_id);
          publish();
        } else if (msg.type === "op_error") {
          // Dropping the op rolls back its optimistic effect — tell the
          // user why instead of leaving a mystery rollback.
          console.warn("[sync] op rejected:", msg.message);
          pendingRef.current = settlePending(pendingRef.current, msg.op_id);
          publish();
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
    // reconnect immediately instead of up to 30 s later. A socket that
    // still claims OPEN is probed rather than trusted: after a screen-off
    // stretch it is usually a corpse.
    const reconnectNow = () => {
      const ws = wsRef.current;
      if (ws && ws.readyState === WebSocket.CONNECTING) return;
      if (ws && ws.readyState === WebSocket.OPEN) {
        const sentAt = Date.now();
        sendPing(ws);
        window.setTimeout(() => {
          if (disposed || wsRef.current !== ws) return;
          // Any frame clears the marker, so a marker no newer than the
          // probe means nothing came back in all that time.
          const outstanding = awaitingPongRef.current;
          if (outstanding !== null && outstanding <= sentAt) abandon(ws);
        }, PROBE_TIMEOUT_MS);
        return;
      }
      clearRetry();
      attemptRef.current = 0;
      connect();
    };
    const onVisibility = () => {
      if (document.visibilityState === "visible") reconnectNow();
    };
    window.addEventListener("online", reconnectNow);
    document.addEventListener("visibilitychange", onVisibility);

    // Steady-state heartbeat. Write the socket off if the last ping went
    // unanswered, else send the next one.
    const heartbeat = window.setInterval(() => {
      const ws = wsRef.current;
      if (!ws || ws.readyState !== WebSocket.OPEN) return;
      if (pingOverdue(awaitingPongRef.current, Date.now())) abandon(ws);
      else if (awaitingPongRef.current === null) sendPing(ws);
    }, HEARTBEAT_INTERVAL_MS);

    connect();

    return () => {
      disposed = true;
      clearRetry();
      window.clearInterval(heartbeat);
      window.removeEventListener("online", reconnectNow);
      document.removeEventListener("visibilitychange", onVisibility);
      wsRef.current?.close();
      wsRef.current = null;
    };
    // `toast` is referentially stable (useCallback in ToastProvider), and
    // `publish` has no deps, so listing them doesn't churn the socket.
  }, [accessToken, toast, publish]);

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

  // Latest queue for `replaceUpcoming`'s source carry-over, readable from
  // a stable callback without re-creating it on every frame.
  const queueItemsRef = useRef(state.playback.queue.items);
  queueItemsRef.current = state.playback.queue.items;

  const submit = useCallback(
    (op: SyncOp) => {
      const entry: PendingOp = { opId: newOpId(), op };
      pendingRef.current = enqueuePending(pendingRef.current, entry);
      publish();
      // Not open yet (connect/reconnect window): `onopen` sends it.
      const ws = wsRef.current;
      if (ws && ws.readyState === WebSocket.OPEN) sendOp(ws, entry);
    },
    [publish],
  );

  const pushTrack = useCallback(
    (track: Track, source?: PlaySource) => {
      const itemId = newItemId();
      trackMetaRef.current.set(track.id, track);
      forceMetaTick((n) => n + 1);
      submit({
        type: "push",
        item_id: itemId,
        track_id: track.id,
        ...(source ? { source } : {}),
      });
      return itemId;
    },
    [submit],
  );

  const startSession = useCallback(
    (tracks: readonly Track[], anchorIndex: number, source?: PlaySource) => {
      if (tracks.length === 0) return "";
      for (const t of tracks) trackMetaRef.current.set(t.id, t);
      forceMetaTick((n) => n + 1);
      const items = tracks.map((t) => ({
        item_id: newItemId(),
        track_id: t.id,
        ...(source ? { source } : {}),
      }));
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
    (
      trackIds: readonly string[],
      meta?: readonly Track[],
      sourceFor?: (trackId: string) => PlaySource | undefined,
    ) => {
      if (meta && meta.length > 0) {
        for (const t of meta) trackMetaRef.current.set(t.id, t);
        forceMetaTick((n) => n + 1);
      }
      const items = sourcedItems(trackIds, newItemId, queueItemsRef.current, sourceFor);
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

function sendOp(ws: WebSocket, { opId, op }: PendingOp): void {
  const msg: ClientMessage = { type: "op", op, op_id: opId };
  ws.send(JSON.stringify(msg));
}

// Op ids only need to be unique among one room's recent ops; the
// gateway caps them at 64 bytes.
function newOpId(): string {
  return `op-${newItemId()}`;
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
