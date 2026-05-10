// Tiny URL-driven router. Multiple components can call useRoute() — they
// all subscribe to the same external store, so navigate() re-renders every
// consumer. The earlier single-`setter` design only updated whichever
// component mounted last, leaving the others stale.

import { ReactNode, useSyncExternalStore } from "react";

const subscribers = new Set<() => void>();

// Snapshot is pathname + search so navigating between /search?q=a and
// /search?q=b re-renders consumers. We return a string (not an object)
// so React's referential bail-out check works without memoization.
function getSnapshot(): string {
  return location.pathname + location.search;
}

function subscribe(cb: () => void): () => void {
  subscribers.add(cb);
  const onPop = () => cb();
  window.addEventListener("popstate", onPop);
  return () => {
    subscribers.delete(cb);
    window.removeEventListener("popstate", onPop);
  };
}

function notify() {
  for (const cb of subscribers) cb();
}

export function navigate(to: string) {
  history.pushState(null, "", to);
  notify();
}

export function useRoute(): {
  path: string;
  search: string;
  pathname: string;
  navigate: (to: string) => void;
} {
  const full = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  const qIdx = full.indexOf("?");
  const pathname = qIdx === -1 ? full : full.slice(0, qIdx);
  const search = qIdx === -1 ? "" : full.slice(qIdx);
  // `path` kept for back-compat with existing callers (App.tsx route matcher,
  // Sidebar.isActive). New code should prefer `pathname` + `search`.
  return { path: pathname, pathname, search, navigate };
}

export function Link({
  to,
  children,
  className,
}: {
  to: string;
  children: ReactNode;
  className?: string;
}) {
  return (
    <a
      href={to}
      className={className}
      onClick={(e) => {
        if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
        e.preventDefault();
        navigate(to);
      }}
    >
      {children}
    </a>
  );
}
