// Pure batching queue for RUM events. Holds up to MAX_BATCH events in
// memory; the caller decides when to flush (interval timer, pagehide,
// or explicit). The HTTP layer is injected so this module is testable
// without a network and reusable from `pagehide` (which prefers
// `fetch(..., {keepalive:true})`).

import { getSessionId } from "./session";

export interface RumEvent {
  session_id: string;
  occurred_ms: number;
  name: string;
  value_ms?: number;
  rating?: "good" | "needs-improvement" | "poor";
  page_path: string;
  fields?: Record<string, unknown>;
}

export interface MarkInput {
  /** Event name, e.g. "playback.start". Web vitals are auto-prefixed
   * with "web-vital." in the web-vitals adapter. */
  name: string;
  /** Optional duration / metric value in ms. Omit for non-timing marks. */
  value_ms?: number;
  rating?: "good" | "needs-improvement" | "poor";
  fields?: Record<string, unknown>;
}

// MAX_BATCH must be ≤ the gateway's `MAX_BATCH` (50). We pick the same
// value so a flood of marks gets *some* of them through rather than
// being dropped wholesale at the gateway.
const MAX_BATCH = 50;

export class RumQueue {
  private buf: RumEvent[] = [];

  push(input: MarkInput): void {
    if (this.buf.length >= MAX_BATCH) {
      // Drop oldest — a misbehaving page emitting 1000 marks/sec
      // shouldn't crowd out the latest signal. We don't track drops
      // because the diagnostics page already shows count-per-name; a
      // gap is visible.
      this.buf.shift();
    }
    this.buf.push({
      session_id: getSessionId(),
      occurred_ms: Date.now(),
      name: input.name,
      ...(input.value_ms !== undefined && { value_ms: input.value_ms }),
      ...(input.rating !== undefined && { rating: input.rating }),
      page_path: window.location.pathname,
      ...(input.fields !== undefined && { fields: input.fields }),
    });
  }

  /** Drain the buffer and return whatever was queued. Caller is
   * responsible for the actual upload — if it fails, the caller can
   * choose whether to re-enqueue or accept the loss. */
  drain(): RumEvent[] {
    const out = this.buf;
    this.buf = [];
    return out;
  }

  size(): number {
    return this.buf.length;
  }
}
