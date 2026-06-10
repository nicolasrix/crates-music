// Admin-only user provisioning UI (PR B). Lists real accounts and lets an
// admin create, delete, and reset-password them. Rendered only inside the
// Account panel when `useIsAdmin()` is true; the gateway independently
// 403s these calls for non-admins, so this is convenience, not security.

import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { KeyRound, Trash2, UserPlus, Users } from "lucide-react";

import {
  AdminUser,
  createUser,
  deleteUser,
  listUsers,
  ProvisionRole,
  resetUserPassword,
} from "../../api/users";
import { useWhoami } from "../../auth/useWhoami";
import { SettingsButton, SettingsSection } from "../controls";

const ICON = { size: 18, strokeWidth: 1.5 } as const;
// Mirrors the gateway's MIN_PASSWORD_LEN so the form fails fast before a
// round-trip. The server is still the authority (it re-checks).
const MIN_PASSWORD_LEN = 12;

const inputStyle = {
  background: "var(--bg-elevated)",
  border: "1px solid var(--border)",
  borderRadius: "var(--radius-1, 2px)",
  color: "var(--fg)",
  padding: "6px 8px",
  fontSize: "0.9rem",
  // Inputs report a wide min-content (≈ their `size` attribute), which would
  // otherwise blow out the grid columns and clip on narrow viewports. Cap
  // them to their cell and count padding inside the width.
  maxWidth: "100%",
  minWidth: 0,
  boxSizing: "border-box" as const,
} as const;

export function UsersAdmin() {
  const qc = useQueryClient();
  const me = useWhoami().data;
  const users = useQuery({ queryKey: ["admin-users"], queryFn: listUsers });

  const invalidate = () => void qc.invalidateQueries({ queryKey: ["admin-users"] });

  return (
    <SettingsSection icon={<Users {...ICON} />} title="users">
      <p className="text-fg-muted text-sm" style={{ marginTop: 0, marginBottom: 16 }}>
        Provision household accounts. Each gets their own queue, taste, and
        playlists. Guests join via a code instead (coming soon).
      </p>

      <CreateUserForm onCreated={invalidate} />

      <div style={{ marginTop: 20 }}>
        {users.isLoading && <p className="text-fg-muted text-sm">Loading…</p>}
        {users.isError && (
          <p className="text-sm" style={{ color: "var(--danger, #e05252)" }}>
            {(users.error as Error).message}
          </p>
        )}
        {users.data?.map((u) => (
          <UserRow key={u.id} user={u} isSelf={u.id === me?.user_id} onChanged={invalidate} />
        ))}
      </div>
    </SettingsSection>
  );
}

function CreateUserForm({ onCreated }: { onCreated: () => void }) {
  const [username, setUsername] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [password, setPassword] = useState("");
  const [role, setRole] = useState<ProvisionRole>("user");

  const create = useMutation({
    mutationFn: () => {
      const dn = displayName.trim();
      // Omit display_name entirely when blank (exactOptionalPropertyTypes
      // forbids passing an explicit `undefined`).
      return createUser({
        username: username.trim(),
        password,
        role,
        ...(dn ? { display_name: dn } : {}),
      });
    },
    onSuccess: () => {
      setUsername("");
      setDisplayName("");
      setPassword("");
      setRole("user");
      onCreated();
    },
  });

  const canSubmit =
    username.trim().length > 0 && password.length >= MIN_PASSWORD_LEN && !create.isPending;

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        if (canSubmit) create.mutate();
      }}
      style={{
        display: "grid",
        gap: 8,
        // auto-fit + a min track width collapses the two columns into one on
        // phone-width panels (where 2×180px can't fit) instead of overflowing.
        gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))",
        alignItems: "end",
      }}
    >
      <label style={{ display: "grid", gap: 4 }}>
        <span className="text-fg-muted text-sm">username</span>
        <input
          style={inputStyle}
          value={username}
          autoCapitalize="none"
          spellCheck={false}
          onChange={(e) => setUsername(e.target.value)}
        />
      </label>
      <label style={{ display: "grid", gap: 4 }}>
        <span className="text-fg-muted text-sm">display name (optional)</span>
        <input
          style={inputStyle}
          value={displayName}
          onChange={(e) => setDisplayName(e.target.value)}
        />
      </label>
      <label style={{ display: "grid", gap: 4 }}>
        <span className="text-fg-muted text-sm">password (min {MIN_PASSWORD_LEN})</span>
        <input
          style={inputStyle}
          type="password"
          value={password}
          autoComplete="new-password"
          onChange={(e) => setPassword(e.target.value)}
        />
      </label>
      <label style={{ display: "grid", gap: 4 }}>
        <span className="text-fg-muted text-sm">role</span>
        <select
          style={inputStyle}
          value={role}
          onChange={(e) => setRole(e.target.value as ProvisionRole)}
        >
          <option value="user">user</option>
          <option value="admin">admin</option>
        </select>
      </label>
      <div style={{ gridColumn: "1 / -1", display: "flex", alignItems: "center", gap: 12 }}>
        <button
          type="submit"
          disabled={!canSubmit}
          className="text-sm"
          style={{
            ...inputStyle,
            display: "inline-flex",
            alignItems: "center",
            gap: 6,
            padding: "6px 10px",
            cursor: canSubmit ? "pointer" : "not-allowed",
            opacity: canSubmit ? 1 : 0.5,
          }}
        >
          <UserPlus size={14} strokeWidth={1.5} />
          add user
        </button>
        {create.isError && (
          <span className="text-sm" style={{ color: "var(--danger, #e05252)" }}>
            {(create.error as Error).message}
          </span>
        )}
      </div>
    </form>
  );
}

function UserRow({
  user,
  isSelf,
  onChanged,
}: {
  user: AdminUser;
  isSelf: boolean;
  onChanged: () => void;
}) {
  const [resetting, setResetting] = useState(false);
  const isOwner = user.id === 1;

  const del = useMutation({
    mutationFn: () => deleteUser(user.id),
    onSuccess: onChanged,
  });

  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 12,
        padding: "8px 0",
        borderBottom: "1px solid var(--border-subtle, var(--border))",
        flexWrap: "wrap",
      }}
    >
      <div style={{ flex: 1, minWidth: 0 }}>
        <span style={{ color: "var(--fg)" }}>{user.display_name || user.username || `#${user.id}`}</span>{" "}
        {user.username && <span className="text-fg-muted text-sm">@{user.username}</span>}
      </div>
      <span
        className="text-sm"
        style={{
          padding: "1px 8px",
          borderRadius: 999,
          border: "1px solid var(--border)",
          color: "var(--fg-muted)",
        }}
      >
        {user.role}
        {isSelf ? " · you" : ""}
      </span>
      <SettingsButton onClick={() => setResetting((v) => !v)}>
        <KeyRound size={14} strokeWidth={1.5} />
        password
      </SettingsButton>
      {/* The owner can't be deleted (the gateway also enforces this). */}
      {!isOwner && (
        <SettingsButton danger onClick={() => del.mutate()}>
          <Trash2 size={14} strokeWidth={1.5} />
          {del.isPending ? "…" : "delete"}
        </SettingsButton>
      )}
      {del.isError && (
        <span className="text-sm" style={{ color: "var(--danger, #e05252)", width: "100%" }}>
          {(del.error as Error).message}
        </span>
      )}
      {resetting && (
        <ResetPassword
          userId={user.id}
          onDone={() => {
            setResetting(false);
            onChanged();
          }}
        />
      )}
    </div>
  );
}

function ResetPassword({ userId, onDone }: { userId: number; onDone: () => void }) {
  const [password, setPassword] = useState("");
  const reset = useMutation({
    mutationFn: () => resetUserPassword(userId, password),
    onSuccess: onDone,
  });
  const canSubmit = password.length >= MIN_PASSWORD_LEN && !reset.isPending;

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        if (canSubmit) reset.mutate();
      }}
      style={{ display: "flex", gap: 8, width: "100%", alignItems: "center", marginTop: 4 }}
    >
      <input
        style={{ ...inputStyle, flex: 1 }}
        type="password"
        placeholder={`new password (min ${MIN_PASSWORD_LEN})`}
        autoComplete="new-password"
        value={password}
        onChange={(e) => setPassword(e.target.value)}
      />
      <button
        type="submit"
        disabled={!canSubmit}
        className="text-sm"
        style={{
          ...inputStyle,
          cursor: canSubmit ? "pointer" : "not-allowed",
          opacity: canSubmit ? 1 : 0.5,
        }}
      >
        {reset.isPending ? "…" : "set"}
      </button>
      {reset.isError && (
        <span className="text-sm" style={{ color: "var(--danger, #e05252)" }}>
          {(reset.error as Error).message}
        </span>
      )}
    </form>
  );
}
