import { describe, expect, it } from "vitest";

import { panelVisible, SETTINGS_ITEMS, visibleGroups } from "./nav";

describe("settings nav role-gating", () => {
  it("admins see every group and item", () => {
    const groups = visibleGroups(true);
    const count = groups.reduce((n, g) => n + g.items.length, 0);
    expect(count).toBe(SETTINGS_ITEMS.length);
  });

  it("non-admins see only config panels, never observe", () => {
    const groups = visibleGroups(false);
    const items = groups.flatMap((g) => g.items);
    expect(items.length).toBeGreaterThan(0);
    expect(items.every((i) => i.kind === "config")).toBe(true);
    // No empty groups leak through.
    expect(groups.every((g) => g.items.length > 0)).toBe(true);
  });

  it("panelVisible mirrors the rail policy", () => {
    // An observe panel id (diagnostics) is admin-only.
    expect(panelVisible("tracing", true)).toBe(true);
    expect(panelVisible("tracing", false)).toBe(false);
    // A config panel is visible to everyone.
    expect(panelVisible("account", false)).toBe(true);
    expect(panelVisible("account", true)).toBe(true);
  });
});
