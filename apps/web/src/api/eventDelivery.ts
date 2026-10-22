// Binds the event outbox (eventOutbox.ts) to localStorage, the
// `/v1/events` transport and the page lifecycle. Kept apart from the pure
// core because the transport pulls in the auth module, which reads
// `location` at load.

import { sendEvents, type RecommendEvent } from "./events";
import { createEventOutbox, type OutboxStore, type QueuedEvent } from "./eventOutbox";

/** Retry cadence while anything is queued. `online` and returning to the
 *  foreground trigger an immediate attempt, so this is only the floor. */
const RETRY_MS = 30_000;

// Inside the `crates-music.` namespace on purpose: clearUserData sweeps
// it when a different user signs in, taking the previous user's
// undelivered events with it.
const STORAGE_KEY = "crates-music.events.outbox";

/** localStorage-backed store. Falls back to memory when storage is
 *  unavailable (private mode, quota), so events still get their live
 *  send attempts — they just won't survive a reload. */
function localOutboxStore(): OutboxStore {
  let memory: QueuedEvent[] = [];
  return {
    read() {
      try {
        const raw = localStorage.getItem(STORAGE_KEY);
        if (raw === null) return memory;
        const parsed: unknown = JSON.parse(raw);
        return Array.isArray(parsed) ? (parsed as QueuedEvent[]) : memory;
      } catch {
        return memory;
      }
    },
    write(queue) {
      memory = queue;
      try {
        if (queue.length === 0) localStorage.removeItem(STORAGE_KEY);
        else localStorage.setItem(STORAGE_KEY, JSON.stringify(queue));
      } catch {
        /* storage unavailable; memory holds it for this page's life */
      }
    },
  };
}

const outbox = createEventOutbox({ send: sendEvents, store: localOutboxStore() });

/** Queue events for delivery. Never throws, never blocks playback. */
export function enqueueEvents(events: readonly RecommendEvent[]): void {
  outbox.enqueue(events);
}

/** Start delivering — call once the signed-in user owns the local data.
 *  Returns a stop function. */
export function startEventOutbox(): () => void {
  outbox.setActive(true);
  const kick = () => void outbox.flush();
  const onVisibility = () => {
    if (document.visibilityState === "visible") kick();
  };
  window.addEventListener("online", kick);
  document.addEventListener("visibilitychange", onVisibility);
  const timer = window.setInterval(kick, RETRY_MS);
  kick();
  return () => {
    outbox.setActive(false);
    window.removeEventListener("online", kick);
    document.removeEventListener("visibilitychange", onVisibility);
    window.clearInterval(timer);
  };
}
