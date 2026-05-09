// Session id generation for RUM. One id per page-load (sessionStorage,
// not localStorage) so a single visit groups its events even when the
// user navigates between routes — but a new tab gets a fresh id, which
// is what the diagnostics page wants for grouping.

const STORAGE_KEY = "rum.session_id";

function newId(): string {
  // 12 random bytes → 16 base64-url chars. Plenty of entropy for the
  // expected scale (one session per page-load); we're not collision-
  // hardening, just disambiguating.
  const bytes = new Uint8Array(12);
  crypto.getRandomValues(bytes);
  let s = "";
  for (const b of bytes) s += b.toString(16).padStart(2, "0");
  return s;
}

export function getSessionId(): string {
  try {
    const existing = sessionStorage.getItem(STORAGE_KEY);
    if (existing) return existing;
    const fresh = newId();
    sessionStorage.setItem(STORAGE_KEY, fresh);
    return fresh;
  } catch {
    // Private mode / disabled storage: fall back to a memo'd value so
    // events at least share an id within this module's lifetime.
    if (!cached) cached = newId();
    return cached;
  }
}

let cached: string | null = null;
