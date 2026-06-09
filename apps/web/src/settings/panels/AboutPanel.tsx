import { Info, Smartphone } from "lucide-react";

import { useInstallPrompt } from "../../pwa/installPrompt";
import { InfoRow, SettingsButton, SettingsSection } from "../controls";

const ICON = { size: 18, strokeWidth: 1.5 } as const;

// Install offer + build stamp. The install button is shown only when
// actionable: hidden once running standalone, and on browsers that neither
// fire beforeinstallprompt nor have a manual path (everything but iOS).
export function AboutPanel() {
  const { canInstall, isStandalone, isIos, promptInstall } = useInstallPrompt();
  const showInstall = !isStandalone && (canInstall || isIos);

  return (
    <SettingsSection icon={<Info {...ICON} />} title="about">
      {showInstall && (
        <div style={{ marginBottom: 18 }}>
          <p className="text-fg-muted text-sm" style={{ marginBottom: 12 }}>
            <Smartphone
              size={16}
              strokeWidth={1.5}
              style={{ verticalAlign: "-3px", marginRight: 6 }}
            />
            Install crates to your home screen / app list — it opens in its own window and launches
            offline.
          </p>
          {canInstall ? (
            <SettingsButton onClick={() => void promptInstall()}>
              <Smartphone size={14} strokeWidth={1.5} />
              install
            </SettingsButton>
          ) : (
            <p className="text-fg-muted text-sm">
              On iOS: open the <strong>Share</strong> menu and choose{" "}
              <strong>Add to Home Screen</strong>.
            </p>
          )}
        </div>
      )}

      <InfoRow label="Build" value={__GIT_SHA__} />
      <InfoRow label="Built" value={new Date(__BUILD_TIME__).toLocaleString()} />
    </SettingsSection>
  );
}
