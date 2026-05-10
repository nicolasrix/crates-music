// Two-column shell: sidebar + main, with the player bar fixed at the bottom
// (rendered as a sibling in App.tsx, not inside Layout — so detail pages
// can mount their own --art-* on <main> without affecting the chrome).
//
// `breadcrumb` is shown in the topbar; pages set it from their own data.

import { ReactNode } from "react";
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
  return (
    <>
      <Sidebar />
      <main>
        <Topbar breadcrumb={breadcrumb} />
        {children}
      </main>
    </>
  );
}
