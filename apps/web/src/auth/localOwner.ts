// Which user the local caches belong to.
//
// The offline audio cache (IndexedDB) and the `crates-music.*` preference
// keys are origin-scoped, not user-scoped — nothing in them records who
// wrote them. Sec review 1.6 closed that bleed by wiping everything on
// sign-out, which is safe but destroys deliberately-downloaded tracks and
// the cache budgets even when the SAME person signs back in — the common
// case on a personal phone, where the shared browser the review worried
// about does not exist.
//
// So we tag the local data with its owning user id and reconcile at
// sign-in instead. A different user signing in still never reads the
// previous occupant's data; a same-user logout/login is lossless.
//
// The trade, stated plainly: the data now survives on disk between
// sign-out and the next sign-in. It stays origin-scoped and the signed-out
// SPA holds no token to serve it, but someone with devtools on this
// browser could read the database directly in that window. On a LAN-only
// household gateway that is the right price for not re-downloading a
// library after every sign-out.

import { clearUserData } from "./userData";

// Deliberately OUTSIDE the `crates-music.` namespace that `clearUserData`
// sweeps, alongside the `gw_*` token keys. The tag describes the data
// rather than being part of it, and it has to outlive a wipe: if a wipe
// erased the tag too, "wiped" would be indistinguishable from "never
// tagged" — and untagged must mean adopt (see reconcileLocalOwner), which
// would turn a half-failed wipe into a silent bleed.
const OWNER_KEY = "gw_local_owner";

/** The user id the local caches belong to, or null when untagged. */
export function readLocalOwner(): number | null {
  try {
    const raw = localStorage.getItem(OWNER_KEY);
    if (!raw) return null;
    const n = Number.parseInt(raw, 10);
    return Number.isFinite(n) ? n : null;
  } catch {
    // Storage unavailable (private mode): report untagged. Reconcile then
    // adopts, which is correct — an unreadable store holds nothing either.
    return null;
  }
}

export function writeLocalOwner(userId: number): void {
  try {
    localStorage.setItem(OWNER_KEY, String(userId));
  } catch {
    /* storage unavailable; nothing to protect in that case either */
  }
}

/** Drop the tag. Pairs with an explicit `clearUserData()` — the caches are
 *  empty afterwards, so the next reconcile should adopt rather than wipe. */
export function clearLocalOwner(): void {
  try {
    localStorage.removeItem(OWNER_KEY);
  } catch {
    /* storage unavailable */
  }
}

/** Claim the local caches for `userId`, wiping first if they belong to
 *  someone else. Resolves true when a wipe happened. Never throws: a failed
 *  reconcile must not block sign-in. */
export async function reconcileLocalOwner(userId: number): Promise<boolean> {
  const owner = readLocalOwner();
  if (owner === userId) return false;
  // Untagged: a first-ever sign-in, the state just after an explicit wipe
  // (guest join), or a cache written by a build that predates tagging. In
  // that last case the old wipe-on-sign-out behaviour guarantees anything
  // still here belongs to whoever was last signed in on this device, so
  // adopting it rather than wiping carries downloads across the upgrade.
  if (owner === null) {
    writeLocalOwner(userId);
    return false;
  }
  try {
    await clearUserData();
  } catch {
    // Leave the previous owner's tag in place so the next sign-in retries
    // the wipe, rather than handing possibly-surviving leftovers to the
    // new user. clearUserData is itself best-effort and effectively never
    // throws; this is the belt to its braces.
    return false;
  }
  writeLocalOwner(userId);
  return true;
}
