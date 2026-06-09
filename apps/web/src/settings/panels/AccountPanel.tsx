import { LogOut, User } from "lucide-react";

import { useAuth } from "../../auth/AuthContext";
import { InfoRow, SettingsButton, SettingsSection } from "../controls";

const ICON = { size: 18, strokeWidth: 1.5 } as const;

export function AccountPanel() {
  const { tokens, logout } = useAuth();

  return (
    <SettingsSection
      icon={<User {...ICON} />}
      title="account"
      action={
        <SettingsButton danger onClick={() => void logout()}>
          <LogOut size={14} strokeWidth={1.5} />
          sign out
        </SettingsButton>
      }
    >
      <InfoRow label="Status" value="Signed in" />
      <InfoRow label="Server" value={location.origin} />
      {tokens && (
        <InfoRow label="Session expires" value={new Date(tokens.expiresAt).toLocaleString()} />
      )}
    </SettingsSection>
  );
}
