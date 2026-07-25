// The chrome every settings/diagnostics panel renders inside: the app
// Layout (sidebar + topbar) wrapping a two-column body — the secondary
// rail + the active panel. Panels are content-only (no <Layout> of their
// own); App.tsx wraps each one as <SettingsShell active="…"><Panel/>.
//
// The breadcrumb is derived from the active item so the topbar reads
// "settings / recommender" without each panel restating it.

import { ReactNode } from "react";

import { Layout } from "../components/Layout";
import { findItem } from "./nav";
import { SettingsRail, SettingsRailSelect } from "./SettingsRail";

export function SettingsShell({ active, children }: { active: string; children: ReactNode }) {
  const item = findItem(active);
  const breadcrumb = item ? `settings / ${item.label}` : "settings";

  return (
    <Layout breadcrumb={breadcrumb} palette={null}>
      <div className="settings-shell">
        <SettingsRail active={active} />
        <SettingsRailSelect active={active} />
        <div className="settings-panel">{children}</div>
      </div>
    </Layout>
  );
}
