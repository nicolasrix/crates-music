import { LogOut, User } from "lucide-react";

import { useAuth } from "../../auth/AuthContext";
import { useIsAdmin, useWhoami } from "../../auth/useWhoami";
import { InfoRow, SettingsButton, SettingsSection } from "../controls";
import { UsersAdmin } from "./UsersAdmin";

const ICON = { size: 18, strokeWidth: 1.5 } as const;

export function AccountPanel() {
  const { tokens, logout } = useAuth();
  const me = useWhoami().data;
  const isAdmin = useIsAdmin();

  return (
    <>
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
        {me && (
          <InfoRow label="Signed in as" value={me.display_name || me.username || `#${me.user_id}`} />
        )}
        {me?.username && <InfoRow label="Username" value={me.username} />}
        {me && <InfoRow label="Role" value={me.role} />}
        <InfoRow label="Server" value={location.origin} />
        {tokens && (
          <InfoRow label="Session expires" value={new Date(tokens.expiresAt).toLocaleString()} />
        )}
      </SettingsSection>

      {/* Provisioning is admin-only; the gateway also enforces this. */}
      {isAdmin && <UsersAdmin />}
    </>
  );
}
