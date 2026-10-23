// Durable outbox for `POST /v1/events`.
//
// Skip and complete events used to be fire-and-forget: one POST, its
// failure swallowed. On a phone that is the common case, not the edge —
// a track ends with the screen off and the radio asleep, and that
// listen never reaches the recommender. (2026-10-22: three of four
// `complete` events in half an hour of screen-off listening were lost.)
//
// So events go to localStorage first and are POSTed from there, leaving
// only once the gateway confirms them. Each carries a `client_event_id`,
// so a retry of a batch that did land — response lost on the way back —
// is de-duplicated server-side instead of counting a skip twice.
//
// Sending waits for `setActive(true)`, which happens only after the
// sign-in owner check (useLocalOwnerReconcile): by then anything still
// queued is this user's, because a different user's leftovers have been
// wiped with the rest of the `crates-music.*` keys.
//
// This module is the pure core, testable without a network or storage;
// eventDelivery.ts binds it to localStorage, fetch and the page lifecycle.

import type { RecommendEvent } from "./events";

/** Well under the gateway's 1000-event ceiling, so one request stays
 *  small enough to get through on a poor link. */
export const MAX_BATCH = 50;
/** Backlog ceiling. Far beyond a day of listening; past it the oldest
 *  events go first, so a device offline for weeks can't grow storage
 *  without bound. */
export const MAX_QUEUED = 500;

export interface QueuedEvent extends RecommendEvent {
  client_event_id: string;
}

export interface OutboxStore {
  read(): QueuedEvent[];
  write(queue: QueuedEvent[]): void;
}

export interface OutboxDeps {
  /** POSTs a batch and resolves to the HTTP status; rejects when the
   *  request never completed (no network). */
  send: (events: QueuedEvent[]) => Promise<number>;
  store: OutboxStore;
  newId?: () => string;
}

export type Delivery = "delivered" | "retry" | "drop";

/** What a response status means for the batch that got it. */
export function classifyStatus(status: number): Delivery {
  if (status >= 200 && status < 300) return "delivered";
  if (status === 408 || status === 429 || status >= 500) return "retry";
  // Any other 4xx is a batch this gateway will never accept. Retrying it
  // would wedge every later event behind it.
  return "drop";
}

export function createEventOutbox({ send, store, newId = defaultId }: OutboxDeps) {
  let active = false;
  let running: Promise<void> | null = null;

  const enqueue = (events: readonly RecommendEvent[]): void => {
    if (events.length === 0) return;
    const queued = events.map((e) => ({ ...e, client_event_id: e.client_event_id ?? newId() }));
    store.write([...store.read(), ...queued].slice(-MAX_QUEUED));
    if (active) void flush();
  };

  // Send oldest-first until the queue is empty or an attempt fails. The
  // queue is re-read every pass (it may have grown meanwhile), and ids
  // already settled in this drain are skipped, so a store whose writes
  // fail can't spin the loop forever.
  const drain = async (): Promise<void> => {
    const settled = new Set<string>();
    while (active) {
      const batch = store
        .read()
        .filter((e) => !settled.has(e.client_event_id))
        .slice(0, MAX_BATCH);
      if (batch.length === 0) return;
      let outcome: Delivery;
      try {
        outcome = classifyStatus(await send(batch));
      } catch {
        outcome = "retry";
      }
      if (outcome === "retry") return;
      if (outcome === "drop") console.warn("[events] gateway rejected a batch; dropping", batch.length);
      for (const e of batch) settled.add(e.client_event_id);
      store.write(store.read().filter((e) => !settled.has(e.client_event_id)));
    }
  };

  /** Drain the queue. Concurrent calls share one drain, which picks up
   *  anything enqueued while it runs. */
  const flush = (): Promise<void> => {
    if (!active) return Promise.resolve();
    running ??= drain().finally(() => {
      running = null;
    });
    return running;
  };

  const setActive = (on: boolean): void => {
    active = on;
  };

  return { enqueue, flush, setActive };
}

function defaultId(): string {
  const c = globalThis.crypto;
  if (c && typeof c.randomUUID === "function") return c.randomUUID();
  return `ev-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}
