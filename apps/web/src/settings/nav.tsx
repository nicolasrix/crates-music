// The settings shell's navigation model: every settings + diagnostics
// surface, grouped by domain. This is the single source of truth — the
// rail (SettingsRail), the mobile <select>, and App.tsx's route table all
// derive from it, so adding a panel is a one-line change here.
//
// "Interleave by concern": each domain group holds both its config panels
// (kind: "config") and the diagnostics that explain it (kind: "observe"),
// so the recommender stats sit next to the autoplay knobs, ingest sits
// next to the cache budgets, etc. The observe items carry a small dot in
// the rail so the two kinds stay visually distinguishable when mixed.

import {
  Activity,
  Boxes,
  Gauge,
  HardDrive,
  Info,
  ListMusic,
  Palette,
  ScatterChart,
  SlidersHorizontal,
  Sparkles,
  User,
  Volume2,
} from "lucide-react";
import { ReactNode } from "react";

export type PanelKind = "config" | "observe";

export interface SettingsItem {
  /** Unique id; also the route slug (/settings/<id>). */
  id: string;
  label: string;
  kind: PanelKind;
  icon: ReactNode;
}

export interface SettingsGroup {
  title: string;
  items: readonly SettingsItem[];
}

const ICON = { size: 16, strokeWidth: 1.5 } as const;

export const SETTINGS_NAV: readonly SettingsGroup[] = [
  {
    title: "general",
    items: [
      { id: "account", label: "account", kind: "config", icon: <User {...ICON} /> },
      { id: "playback", label: "playback", kind: "config", icon: <Volume2 {...ICON} /> },
      { id: "appearance", label: "appearance", kind: "config", icon: <Palette {...ICON} /> },
    ],
  },
  {
    title: "recommendations",
    items: [
      { id: "autoplay", label: "autoplay", kind: "config", icon: <SlidersHorizontal {...ICON} /> },
      { id: "recommender", label: "recommender", kind: "observe", icon: <Sparkles {...ICON} /> },
      { id: "listening", label: "listening", kind: "observe", icon: <ListMusic {...ICON} /> },
      { id: "latent", label: "latent space", kind: "observe", icon: <ScatterChart {...ICON} /> },
    ],
  },
  {
    title: "storage",
    items: [
      { id: "storage", label: "offline & cache", kind: "config", icon: <HardDrive {...ICON} /> },
      { id: "ingest", label: "ingest", kind: "observe", icon: <Boxes {...ICON} /> },
    ],
  },
  {
    title: "system",
    items: [
      { id: "about", label: "about", kind: "config", icon: <Info {...ICON} /> },
      { id: "tracing", label: "tracing", kind: "observe", icon: <Activity {...ICON} /> },
      { id: "rum", label: "client RUM", kind: "observe", icon: <Gauge {...ICON} /> },
    ],
  },
];

/** Flat list of every panel, in rail order. */
export const SETTINGS_ITEMS: readonly SettingsItem[] = SETTINGS_NAV.flatMap((g) => g.items);

/** Where /settings (bare) and the sidebar's "settings" link land. */
export const DEFAULT_PANEL = "account";

export function panelRoute(id: string): string {
  return `/settings/${id}`;
}

export function findItem(id: string): SettingsItem | undefined {
  return SETTINGS_ITEMS.find((i) => i.id === id);
}
