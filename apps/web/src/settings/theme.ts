// Colour theme, persisted in localStorage and applied to <html data-theme>.
//
// The token system (styles/tokens.css) already defines three states:
//   • no [data-theme]            → dark by default, but a
//     `prefers-color-scheme: light` media query flips it → this is "system".
//   • [data-theme="dark"]        → forced dark.
//   • [data-theme="light"]       → forced light.
//
// So all this module does is set/clear the attribute and remember the choice.
// applyTheme() is called once from main.tsx before React renders (no flash)
// and again whenever the Settings selector changes.

export type Theme = "system" | "dark" | "light";

export const THEMES: readonly Theme[] = ["system", "dark", "light"];

const STORAGE_KEY = "crates-music.theme";

export function loadTheme(): Theme {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    return THEMES.includes(v as Theme) ? (v as Theme) : "system";
  } catch {
    return "system";
  }
}

export function applyTheme(theme: Theme): void {
  const root = document.documentElement;
  if (theme === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", theme);
}

export function saveTheme(theme: Theme): void {
  try {
    localStorage.setItem(STORAGE_KEY, theme);
  } catch {
    /* localStorage may be unavailable (private mode); ignore */
  }
  applyTheme(theme);
}
