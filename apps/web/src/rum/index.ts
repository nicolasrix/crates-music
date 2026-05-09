// RUM bootstrap. Call `initRum()` once after the user is signed in to:
//   * subscribe to web-vitals (LCP / INP / CLS / FCP / TTFB) and turn
//     each into a `web-vital.<NAME>` mark
//   * flush the in-memory queue every FLUSH_MS
//   * flush on `pagehide` via keepalive fetch (the only chance to
//     deliver "session ended at LCP=N" before the tab dies)
//
// Custom marks are emitted by app code via `markEvent("name", ...)`,
// re-exported from this module.

import { onCLS, onFCP, onINP, onLCP, onTTFB, type Metric } from "web-vitals";
import { RumQueue, type MarkInput } from "./queue";
import { uploadEvents, uploadEventsBeacon } from "./upload";

const FLUSH_MS = 10_000;

let queue: RumQueue | null = null;
let flushTimer: number | null = null;

function ensureQueue(): RumQueue {
  if (!queue) queue = new RumQueue();
  return queue;
}

/** Public mark API. Safe to call before `initRum()` — the event will
 * sit in the in-memory queue and ship on the first flush. */
export function markEvent(name: string, opts: Omit<MarkInput, "name"> = {}): void {
  ensureQueue().push({ name, ...opts });
}

function reportVital(metric: Metric): void {
  // exactOptionalPropertyTypes: only include `rating` when the
  // web-vitals library actually supplies one.
  const input: MarkInput = {
    name: `web-vital.${metric.name}`,
    value_ms: metric.value,
    fields: { id: metric.id, navigationType: metric.navigationType },
    ...(metric.rating !== undefined && { rating: metric.rating }),
  };
  ensureQueue().push(input);
}

async function flushNow(): Promise<void> {
  const events = ensureQueue().drain();
  await uploadEvents(events);
}

function flushOnPagehide(): void {
  // Drain synchronously and dispatch a keepalive fetch. We deliberately
  // do NOT await — the page is going away.
  const events = ensureQueue().drain();
  uploadEventsBeacon(events);
}

let initialized = false;

export function initRum(): void {
  if (initialized) return;
  initialized = true;

  // Web vitals: each `on*` fires at most once per page-load, except
  // CLS/INP which can update as the user interacts. The library
  // handles deduping; we just push each report.
  onCLS(reportVital);
  onFCP(reportVital);
  onINP(reportVital);
  onLCP(reportVital);
  onTTFB(reportVital);

  // Periodic flush. Skipped if the queue is empty — `uploadEvents` is
  // a no-op in that case.
  flushTimer = window.setInterval(() => {
    void flushNow();
  }, FLUSH_MS);

  // Pagehide is the most reliable "tab going away" signal — covers
  // back-forward cache, mobile tab killers, and ordinary close.
  // visibilitychange→hidden is a softer trigger we also flush on,
  // because Safari historically didn't always fire pagehide on iOS
  // background.
  window.addEventListener("pagehide", flushOnPagehide);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") flushOnPagehide();
  });
}

/** Tear-down for tests / hot reload. */
export function shutdownRum(): void {
  if (flushTimer !== null) {
    clearInterval(flushTimer);
    flushTimer = null;
  }
  initialized = false;
  queue = null;
}
