// Reconcile the local caches' owner once identity is known.
//
// Mounted app-wide by <App/> rather than run from completeLogin(), for two
// reasons: the token response carries no user id (only /v1/whoami resolves
// the principal), and an installed PWA's normal lifecycle is
// relaunch-from-stored-token, which never passes through the callback at
// all. Keyed on the resolved user id, so it fires once per identity.
//
// It rides the existing ["whoami"] query, so this costs no extra request —
// TanStack dedupes it against every other useWhoami() consumer.

import { useEffect } from "react";

import { reconcileLocalOwner } from "./localOwner";
import { useWhoami } from "./useWhoami";

export function useLocalOwnerReconcile(): void {
  const userId = useWhoami().data?.user_id;
  useEffect(() => {
    if (userId === undefined) return;
    // Fire-and-forget: a wipe races the first cache reads of the new
    // session by however long whoami takes. Awaiting it instead would mean
    // gating playback on the network, and AudioCacheContext's
    // gesture-critical primePlayback path must resolve synchronously.
    void reconcileLocalOwner(userId);
  }, [userId]);
}
