// Captures the browser's `beforeinstallprompt` event so the app can offer
// "install" from its own UI (Settings) instead of relying on the user
// finding the browser-menu entry. The event fires once, shortly after
// load — this module must be imported for its side effect *before* React
// mounts (main.tsx imports it ahead of render).
//
// iOS Safari never fires the event; install there is always manual
// (Share → Add to Home Screen), so the UI shows a hint instead.

import { useSyncExternalStore } from "react";

// Chromium-only event; not in lib.dom.d.ts.
type BeforeInstallPromptEvent = Event & {
  prompt: () => Promise<void>;
  userChoice: Promise<{ outcome: "accepted" | "dismissed"; platform: string }>;
};

let deferred: BeforeInstallPromptEvent | null = null;
const listeners = new Set<() => void>();

function notify() {
  for (const fn of listeners) fn();
}

/** Test seam + the real event handler. Exported for unit tests. */
export function captureInstallPrompt(e: Event) {
  // Suppress Chrome's mini-infobar; we surface our own button.
  e.preventDefault();
  deferred = e as BeforeInstallPromptEvent;
  notify();
}

if (typeof window !== "undefined") {
  window.addEventListener("beforeinstallprompt", captureInstallPrompt);
  window.addEventListener("appinstalled", () => {
    deferred = null;
    notify();
  });
}

function subscribe(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

function canInstallNow(): boolean {
  return deferred !== null;
}

/** True when already running as an installed app (standalone window). */
export function isStandalone(): boolean {
  if (typeof window === "undefined") return false;
  return (
    window.matchMedia("(display-mode: standalone)").matches ||
    // Legacy iOS signal, predates the display-mode media query.
    (navigator as Navigator & { standalone?: boolean }).standalone === true
  );
}

/** iOS never fires beforeinstallprompt — install is manual there. */
export function isIos(): boolean {
  if (typeof navigator === "undefined") return false;
  return /iPad|iPhone|iPod/.test(navigator.userAgent);
}

/**
 * Show the browser's install dialog. The captured event is single-use:
 * after prompting we drop it (Chrome re-fires beforeinstallprompt on a
 * later visit if the user dismissed).
 */
export async function promptInstall(): Promise<
  "accepted" | "dismissed" | "unavailable"
> {
  const ev = deferred;
  if (!ev) return "unavailable";
  deferred = null;
  notify();
  await ev.prompt();
  const choice = await ev.userChoice;
  return choice.outcome;
}

/** Reactive view for components: re-renders when capture/consume happens. */
export function useInstallPrompt() {
  const canInstall = useSyncExternalStore(
    subscribe,
    canInstallNow,
    () => false,
  );
  return { canInstall, isStandalone: isStandalone(), isIos: isIos(), promptInstall };
}
