import { describe, expect, it } from "vitest";
import type { RecommendEvent } from "./events";
import {
  MAX_BATCH,
  MAX_QUEUED,
  classifyStatus,
  createEventOutbox,
  type OutboxStore,
  type QueuedEvent,
} from "./eventOutbox";

class MemoryStore implements OutboxStore {
  queue: QueuedEvent[] = [];
  read(): QueuedEvent[] {
    return this.queue;
  }
  write(queue: QueuedEvent[]): void {
    this.queue = queue;
  }
}

const ev = (track: string): RecommendEvent => ({
  event_type: "complete",
  track_id: track,
  occurred_at: 1_000,
});

let seq = 0;
const newId = () => `ev-${(seq += 1)}`;

/** A send stub that records every batch and answers from `replies`
 *  (a status, or "offline" to reject like fetch does with no network). */
function sender(replies: (number | "offline")[] = []) {
  const batches: QueuedEvent[][] = [];
  const send = async (events: QueuedEvent[]) => {
    batches.push(events);
    const r = replies.length > 0 ? replies.shift()! : 202;
    if (r === "offline") throw new TypeError("Failed to fetch");
    return r;
  };
  return { send, batches };
}

describe("classifyStatus", () => {
  it("delivers on 2xx", () => {
    expect(classifyStatus(202)).toBe("delivered");
  });
  it("retries what a later attempt could fix", () => {
    for (const s of [408, 429, 500, 502, 503, 504]) expect(classifyStatus(s)).toBe("retry");
  });
  it("drops a batch the gateway will never accept", () => {
    // Retrying a malformed batch forever would wedge every event behind it.
    for (const s of [400, 413, 422]) expect(classifyStatus(s)).toBe("drop");
  });
});

describe("event outbox", () => {
  it("persists an event before trying to send it", async () => {
    // The page can be killed mid-request; the event must already be on
    // disk so the next launch sends it.
    const store = new MemoryStore();
    const { send } = sender(["offline"]);
    const outbox = createEventOutbox({ send, store, newId });
    outbox.setActive(true);
    outbox.enqueue([ev("t1")]);
    expect(store.queue).toHaveLength(1);
    expect(store.queue[0]!.client_event_id).toMatch(/^ev-/);
    await outbox.flush();
  });

  it("keeps an event the network swallowed and delivers it later", async () => {
    // Today's bug: a track ends with the phone's radio asleep, the POST
    // fails, and the `complete` is gone for good.
    const store = new MemoryStore();
    const { send, batches } = sender(["offline", 202]);
    const outbox = createEventOutbox({ send, store, newId });
    outbox.setActive(true);
    outbox.enqueue([ev("t1")]);
    await outbox.flush();
    expect(store.queue).toHaveLength(1);

    await outbox.flush();
    expect(store.queue).toHaveLength(0);
    // Same id both times, so the gateway can de-duplicate a retry whose
    // first attempt did land.
    expect(batches[1]![0]!.client_event_id).toBe(batches[0]![0]!.client_event_id);
  });

  it("keeps the batch on a retryable status, drops it on a fatal one", async () => {
    const store = new MemoryStore();
    const { send } = sender([503, 400]);
    const outbox = createEventOutbox({ send, store, newId });
    outbox.setActive(true);
    outbox.enqueue([ev("t1")]);
    await outbox.flush();
    expect(store.queue).toHaveLength(1);
    await outbox.flush();
    expect(store.queue).toHaveLength(0);
  });

  it("sends nothing until activated", async () => {
    // Activation waits for the sign-in owner check, so a previous user's
    // events can never go out under the new user's token.
    const store = new MemoryStore();
    const { send, batches } = sender();
    const outbox = createEventOutbox({ send, store, newId });
    outbox.enqueue([ev("t1")]);
    await outbox.flush();
    expect(batches).toHaveLength(0);
    expect(store.queue).toHaveLength(1);

    outbox.setActive(true);
    await outbox.flush();
    expect(batches).toHaveLength(1);
    expect(store.queue).toHaveLength(0);
  });

  it("drains a backlog in gateway-sized batches", async () => {
    const store = new MemoryStore();
    const { send, batches } = sender();
    const outbox = createEventOutbox({ send, store, newId });
    outbox.enqueue(Array.from({ length: MAX_BATCH * 2 + 5 }, (_, i) => ev(`t${i}`)));
    outbox.setActive(true);
    await outbox.flush();
    expect(batches.map((b) => b.length)).toEqual([MAX_BATCH, MAX_BATCH, 5]);
    expect(store.queue).toHaveLength(0);
  });

  it("never runs two sends at once, and sends what arrived meanwhile", async () => {
    const store = new MemoryStore();
    let inFlight = 0;
    let maxInFlight = 0;
    const sent: string[] = [];
    let release: () => void = () => {};
    const gate = new Promise<void>((r) => (release = r));
    const send = async (events: QueuedEvent[]) => {
      inFlight += 1;
      maxInFlight = Math.max(maxInFlight, inFlight);
      if (sent.length === 0) await gate;
      sent.push(...events.map((e) => e.track_id));
      inFlight -= 1;
      return 202;
    };
    const outbox = createEventOutbox({ send, store, newId });
    outbox.setActive(true);
    outbox.enqueue([ev("t1")]);
    const first = outbox.flush();
    outbox.enqueue([ev("t2")]);
    const second = outbox.flush();
    release();
    await Promise.all([first, second]);
    expect(maxInFlight).toBe(1);
    expect(sent).toEqual(["t1", "t2"]);
    expect(store.queue).toHaveLength(0);
  });

  it("caps the backlog by dropping the oldest events", () => {
    const store = new MemoryStore();
    const outbox = createEventOutbox({ send: sender().send, store, newId });
    outbox.enqueue(Array.from({ length: MAX_QUEUED + 3 }, (_, i) => ev(`t${i}`)));
    expect(store.queue).toHaveLength(MAX_QUEUED);
    expect(store.queue[0]!.track_id).toBe("t3");
  });

  it("removes only what it sent when the queue changed mid-send", async () => {
    // Another tab (or this one) may append while a POST is out; a delivered
    // batch must not take those with it.
    const store = new MemoryStore();
    const sent: string[] = [];
    const outbox = createEventOutbox({
      send: async (events) => {
        if (sent.length === 0) {
          store.queue = [...store.queue, { ...ev("late"), client_event_id: "ev-late" }];
        }
        sent.push(...events.map((e) => e.track_id));
        return 202;
      },
      store,
      newId,
    });
    outbox.enqueue([ev("t1")]);
    outbox.setActive(true);
    await outbox.flush();
    // `late` arrived after the first batch was taken; the same drain sent
    // it on its next pass, so it was neither lost nor dropped unsent.
    expect(sent).toEqual(["t1", "late"]);
    expect(store.queue).toHaveLength(0);
  });
});
