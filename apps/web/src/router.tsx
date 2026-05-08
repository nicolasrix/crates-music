// Tiny URL-driven router. Three routes today (home, album detail,
// OAuth callback) — TanStack Router would be overkill; this is fewer
// than 30 lines and has no third-party surface to learn.

import { ReactNode, useEffect, useState } from "react";

interface RouteState {
  path: string;
  navigate: (to: string) => void;
}

let setter: ((p: string) => void) | null = null;

export function navigate(to: string) {
  history.pushState(null, "", to);
  setter?.(to);
}

export function useRoute(): RouteState {
  const [path, setPath] = useState(location.pathname);
  useEffect(() => {
    setter = setPath;
    const onPop = () => setPath(location.pathname);
    window.addEventListener("popstate", onPop);
    return () => {
      setter = null;
      window.removeEventListener("popstate", onPop);
    };
  }, []);
  return { path, navigate };
}

export function Link({ to, children, className }: { to: string; children: ReactNode; className?: string }) {
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
