// Local user-data wipe for account switches on a shared device (sec
// review 1.6). The offline audio cache (IndexedDB, including pinned
// tracks) and the app's preference keys in localStorage otherwise survive
// a logout / user-switch, so the next person to sign in on the same
// browser would inherit the previous user's cached library and settings.
//
// Auth tokens are cleared separately by `clearTokens()`. The RUM session
// id (`rum.session_id`) is a per-page correlation id, not user data, and
// is deliberately left untouched. The queue is server-owned (per-user
// sync room) and re-fetched on sign-in, so it is not a local bleed vector.

import { getAudioCache } from "../cache/audioCache";

/** localStorage key namespaces holding user preferences/state. Swept by
 *  prefix so future `crates-music.*` keys are covered automatically. */
const USER_DATA_PREFIXES = ["crates-music."];

/** Individually-named user keys that fall outside the namespaces above. */
const USER_DATA_KEYS = ["player.volume"];

function isUserDataKey(key: string): boolean {
  return USER_DATA_PREFIXES.some((p) => key.startsWith(p)) || USER_DATA_KEYS.includes(key);
}

/** Wipe everything tied to the current user's local session: the offline
 *  audio cache and all preference keys. Best-effort — a failure to delete
 *  the IndexedDB database must not block sign-out. */
export async function clearUserData(): Promise<void> {
  try {
    await getAudioCache().wipe();
  } catch {
    /* best-effort; still clear localStorage below */
  }
  const toRemove: string[] = [];
  for (let i = 0; i < localStorage.length; i++) {
    const k = localStorage.key(i);
    if (k && isUserDataKey(k)) toRemove.push(k);
  }
  for (const k of toRemove) localStorage.removeItem(k);
}
