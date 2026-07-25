// Two-column shell: sidebar + main, with the player bar fixed at the bottom
// (rendered as a sibling in App.tsx, not inside Layout — so detail pages
// can mount their own --art-* on <main> without affecting the chrome).
//
// `breadcrumb` is shown in the topbar; pages set it from their own data.
//
// On phones (≤768px, see the responsive section of components.css) the
// sidebar becomes an off-canvas drawer; Layout owns its open state and
// renders the tap-to-dismiss backdrop. The state lives here (not app-
// global) deliberately: route changes that remount Layout drop it back
// to closed, and same-page navigations close it via Sidebar's onClose.

import { ReactNode, useEffect, useState } from "react";
import { Sidebar } from "./Sidebar";
import { Topbar } from "./Topbar";
import { useArtworkOnMain, type Palette } from "./ArtworkPalette";

interface LayoutProps {
  children: ReactNode;
  breadcrumb?: string;
  /** When set, four CSS variables are written to <main> from this palette,
   *  driving the per-page artwork tint. Pass null on chrome-only pages. */
  palette?: Palette | null;
}

export function Layout({ children, breadcrumb, palette }: LayoutProps) {
  useArtworkOnMain(palette ?? null);
  const [navOpen, setNavOpen] = useState(false);

  // Escape closes the drawer — only listening while it's open.
  useEffect(() => {
    if (!navOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setNavOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [navOpen]);

  return (
    <>
      <Sidebar open={navOpen} onClose={() => setNavOpen(false)} />
      {navOpen && (
        <div
          className="sidebar-backdrop"
          onClick={() => setNavOpen(false)}
          aria-hidden="true"
        />
      )}
      <main>
        <Topbar
          breadcrumb={breadcrumb}
          navOpen={navOpen}
          onMenu={() => setNavOpen(true)}
        />
        {children}
      </main>
    </>
  );
}
