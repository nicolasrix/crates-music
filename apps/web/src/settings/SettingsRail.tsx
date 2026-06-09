// Secondary navigation for the settings shell. Two presentations of the
// same SETTINGS_NAV model:
//   - SettingsRail: a vertical, grouped rail (desktop / ≥768px).
//   - SettingsRailSelect: a grouped <select> (phones), so 12 panels stay
//     reachable without a 12-row scroller eating the viewport.
// CSS toggles which one shows at the 768px breakpoint.

import { Link, navigate } from "../router";
import { panelRoute, SETTINGS_NAV } from "./nav";

export function SettingsRail({ active }: { active: string }) {
  return (
    <nav className="settings-rail" aria-label="settings sections">
      {SETTINGS_NAV.map((group) => (
        <div className="settings-rail-group" key={group.title}>
          <div className="settings-rail-head">{group.title}</div>
          {group.items.map((item) => (
            <Link
              key={item.id}
              to={panelRoute(item.id)}
              className={[
                "settings-rail-item",
                item.kind === "observe" ? "is-observe" : "",
                active === item.id ? "is-active" : "",
              ]
                .filter(Boolean)
                .join(" ")}
            >
              <span className="settings-rail-icon">{item.icon}</span>
              <span className="settings-rail-label">{item.label}</span>
              {item.kind === "observe" && (
                <span className="settings-rail-dot" title="diagnostics" aria-hidden="true" />
              )}
            </Link>
          ))}
        </div>
      ))}
    </nav>
  );
}

export function SettingsRailSelect({ active }: { active: string }) {
  return (
    <div className="settings-rail-mobile">
      <select
        className="settings-rail-select"
        value={active}
        onChange={(e) => navigate(panelRoute(e.target.value))}
        aria-label="settings section"
      >
        {SETTINGS_NAV.map((group) => (
          <optgroup key={group.title} label={group.title}>
            {group.items.map((item) => (
              <option key={item.id} value={item.id}>
                {item.label}
                {item.kind === "observe" ? " ·" : ""}
              </option>
            ))}
          </optgroup>
        ))}
      </select>
    </div>
  );
}
