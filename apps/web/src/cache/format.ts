// Human-readable byte sizes for the cache UI (stats, budgets, tooltips).

const UNITS = ["B", "KB", "MB", "GB", "TB"];

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 MB";
  const i = Math.min(UNITS.length - 1, Math.floor(Math.log(bytes) / Math.log(1024)));
  const value = bytes / 1024 ** i;
  // No decimals for bytes/KB; one decimal for MB+; drop a trailing ".0".
  const digits = i >= 2 ? 1 : 0;
  return `${value.toFixed(digits).replace(/\.0$/, "")} ${UNITS[i]}`;
}
