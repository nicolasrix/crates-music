/** "m:ss" or "h:mm:ss" — the README pins this and forbids "2 minutes 15 seconds". */
export function fmtDuration(seconds: number | null | undefined): string {
  if (!Number.isFinite(seconds ?? NaN) || (seconds ?? 0) < 0) return "0:00";
  const total = Math.floor(seconds!);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  if (h > 0) return `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
  return `${m}:${String(s).padStart(2, "0")}`;
}

/** Compact ms — diagnostics tables. "<1ms", "12ms", "1.20s". */
export function fmtMs(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n)) return "—";
  if (n < 1) return "<1ms";
  if (n < 1000) return `${Math.round(n)}ms`;
  return `${(n / 1000).toFixed(2)}s`;
}

/** Pluralised play count, or null when there's nothing useful to show.
 *  null is the "render nothing" signal; callers use it in conditional
 *  JSX (`{fmtPlays(...) && <span>{fmtPlays(...)}</span>}`) so the
 *  surrounding "·" separator can be elided cleanly. */
export function fmtPlays(n: number | null | undefined): string | null {
  if (n == null || !Number.isFinite(n) || n <= 0) return null;
  const rounded = Math.floor(n);
  return rounded === 1 ? "1 play" : `${rounded} plays`;
}

/** Coarse relative timestamp, e.g. "today", "yesterday", "3 days ago".
 *  Tuned for "last played" surfacing where minute-level precision is
 *  noise — the user cares whether they listened today, this week,
 *  this month, or longer ago. Returns null on missing/unparseable input.
 *  `now` parameter is for testability; defaults to Date.now(). */
export function fmtRelativePast(
  iso: string | null | undefined,
  now: number = Date.now()
): string | null {
  if (!iso) return null;
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return null;
  const diffMs = now - t;
  if (diffMs < 0) {
    // Future timestamps are nonsensical for "last played" but we don't
    // want to crash the page on a clock-skew bug — just bucket as "today".
    return "today";
  }
  const SEC = 1000;
  const MIN = 60 * SEC;
  const HOUR = 60 * MIN;
  const DAY = 24 * HOUR;
  if (diffMs < HOUR) return "just now";
  if (diffMs < DAY) {
    // Crossing midnight isn't the boundary — Subsonic's `played` is a
    // wall-clock instant, so we count elapsed hours, not calendar
    // days. "today" reads better than "23 hours ago" for fresh plays.
    return "today";
  }
  if (diffMs < 2 * DAY) return "yesterday";
  const days = Math.floor(diffMs / DAY);
  if (days < 7) return `${days} days ago`;
  if (days < 14) return "a week ago";
  if (days < 60) return `${Math.floor(days / 7)} weeks ago`;
  if (days < 365) return `${Math.floor(days / 30)} months ago`;
  const years = Math.floor(days / 365);
  return years === 1 ? "a year ago" : `${years} years ago`;
}

/** Bytes → "1.2 GB", etc. */
export function fmtBytes(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n)) return "—";
  const units = ["B", "kB", "MB", "GB", "TB"];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v.toFixed(v < 10 && i > 0 ? 1 : 0)} ${units[i]}`;
}
