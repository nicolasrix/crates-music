// The client's ledger of ops the gateway has not acknowledged yet, kept
// pure so it can be exercised without a socket (same shape as
// reconnect.ts / apply.ts).
//
// What the UI shows is the last state the gateway confirmed with every
// pending op folded over it. That is what lets a track end advance the
// queue with no network at all — a phone with its screen off routinely
// has none — and what keeps a reconnect snapshot, which predates those
// ops, from yanking playback back to where it was. An op leaves the
// ledger when its `applied` echo, `ack` (already applied, resent after a
// reconnect) or `op_error` comes back carrying its id.

import { applyOp } from "./apply";
import type { SyncOp, SyncState } from "./types";

export interface PendingOp {
  /** Client-minted id the gateway echoes back, matching ack to op. */
  opId: string;
  op: SyncOp;
}

// Absolute, last-writer-wins setters: a second one of the same kind
// wholly replaces the first.
const SUPERSEDING: ReadonlySet<SyncOp["type"]> = new Set([
  "set_now_playing",
  "set_playing",
  "set_position",
]);

/**
 * Append `next`, replacing the previous entry when both are the same
 * absolute setter. Only an *adjacent* one is replaced: an op in between
 * may depend on the superseded value (`replace_upcoming` keeps the queue
 * up to the cursor), so collapsing across it would change the result.
 */
export function enqueuePending(pending: readonly PendingOp[], next: PendingOp): PendingOp[] {
  const last = pending[pending.length - 1];
  if (last && SUPERSEDING.has(next.op.type) && last.op.type === next.op.type) {
    return [...pending.slice(0, -1), next];
  }
  return [...pending, next];
}

/** Drop the op `opId` names. Unknown ids — another device's op, or one
 *  this client collapsed away — leave the ledger as it is. */
export function settlePending(
  pending: readonly PendingOp[],
  opId: string | undefined,
): readonly PendingOp[] {
  if (opId === undefined || !pending.some((p) => p.opId === opId)) return pending;
  return pending.filter((p) => p.opId !== opId);
}

/** What to render: `confirmed` with each pending op applied in order.
 *  The version stays the confirmed one — this state is not acked. */
export function projectState(confirmed: SyncState, pending: readonly PendingOp[]): SyncState {
  return pending.reduce((s, p) => applyOp(s, p.op, confirmed.version), confirmed);
}
