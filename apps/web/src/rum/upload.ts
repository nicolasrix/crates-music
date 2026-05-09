// HTTP upload for RUM batches. Two paths:
//   * `uploadEvents` — regular fetch with auth header; used by the
//     interval timer.
//   * `uploadEventsBeacon` — `fetch(..., {keepalive:true})` so the
//     request survives `pagehide`. Uses the same auth header (modern
//     browsers support keepalive + custom headers; sendBeacon does
//     not, which is why we don't use it).
//
// We deliberately ignore failures here: RUM is best-effort, and a
// failed upload during pagehide can't be retried anyway. Logging
// during pagehide is also unsafe — the page is unloading.

import { readTokens } from "../auth/tokens";
import type { RumEvent } from "./queue";

const ENDPOINT = "/v1/diagnostics/client_events";

export async function uploadEvents(events: RumEvent[]): Promise<void> {
  if (events.length === 0) return;
  const tokens = readTokens();
  if (!tokens) return; // not signed in: drop silently.
  try {
    await fetch(ENDPOINT, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        authorization: `Bearer ${tokens.accessToken}`,
      },
      body: JSON.stringify({ events }),
    });
  } catch {
    // best-effort
  }
}

export function uploadEventsBeacon(events: RumEvent[]): void {
  if (events.length === 0) return;
  const tokens = readTokens();
  if (!tokens) return;
  try {
    // `keepalive:true` lets the browser deliver the request even after
    // the page is unloaded. Body capped to 64 KB by spec — our typical
    // batch is ≪ 1 KB so we don't bother chunking.
    void fetch(ENDPOINT, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        authorization: `Bearer ${tokens.accessToken}`,
      },
      body: JSON.stringify({ events }),
      keepalive: true,
    });
  } catch {
    // best-effort during unload
  }
}
