import { Palette } from "lucide-react";
import { useState } from "react";

import { SelectRow, SettingsSection } from "../controls";
import { loadTheme, saveTheme, Theme } from "../theme";

const ICON = { size: 18, strokeWidth: 1.5 } as const;

const THEME_OPTIONS: { value: Theme; label: string }[] = [
  { value: "system", label: "System" },
  { value: "dark", label: "Dark" },
  { value: "light", label: "Light" },
];

export function AppearancePanel() {
  const [theme, setTheme] = useState<Theme>(loadTheme);

  const update = (t: Theme) => {
    setTheme(t);
    saveTheme(t); // also applies <html data-theme> immediately
  };

  return (
    <SettingsSection icon={<Palette {...ICON} />} title="appearance">
      <SelectRow
        id="theme"
        label="Theme"
        value={theme}
        options={THEME_OPTIONS}
        help="System follows your device's light/dark preference."
        onChange={update}
      />
    </SettingsSection>
  );
}
