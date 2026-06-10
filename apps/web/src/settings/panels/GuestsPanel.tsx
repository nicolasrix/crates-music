// Host-side guest-code management (PR D of the user-system plan).
//
// A real account (admin or user) mints shareable codes; a visitor redeems
// one on the sign-in screen ("join with a guest code") and lands in *this*
// host's room as an ephemeral guest. The plaintext code is shown exactly
// once at creation — only its hash is stored — so we surface it in a
// copy-it-now banner and never again.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Copy, KeyRound, Plus, Trash2 } from "lucide-react";
import { useState } from "react";

import {
  CreatedGuestCode,
  GuestCode,
  createGuestCode,
  listGuestCodes,
  revokeGuestCode,
} from "../../api/client";
import { SettingsButton, SettingsSection } from "../controls";

const ICON = { size: 18, strokeWidth: 1.5 } as const;

const EXPIRY_OPTIONS: readonly { label: string; seconds: number | undefined }[] = [
  { label: "12 hours", seconds: 12 * 60 * 60 },
  { label: "1 day", seconds: 24 * 60 * 60 },
  { label: "1 week", seconds: 7 * 24 * 60 * 60 },
  { label: "never (revoke to end)", seconds: undefined },
];

export function GuestsPanel() {
  const qc = useQueryClient();
  const codes = useQuery<GuestCode[]>({ queryKey: ["guest_codes"], queryFn: listGuestCodes });

  const [label, setLabel] = useState("");
  const [expiryIdx, setExpiryIdx] = useState(0);
  const [maxUses, setMaxUses] = useState("");
  const [fresh, setFresh] = useState<CreatedGuestCode | null>(null);

  const create = useMutation({
    mutationFn: () =>
      createGuestCode({
        label: label.trim() || undefined,
        expiresInSeconds: EXPIRY_OPTIONS[expiryIdx]?.seconds,
        maxUses: maxUses.trim() ? Math.max(1, Number(maxUses)) : undefined,
      }),
    onSuccess: (c) => {
      setFresh(c);
      setLabel("");
      setMaxUses("");
      void qc.invalidateQueries({ queryKey: ["guest_codes"] });
    },
  });

  const revoke = useMutation({
    mutationFn: (id: number) => revokeGuestCode(id),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["guest_codes"] }),
  });

  return (
    <SettingsSection icon={<KeyRound {...ICON} />} title="guests">
      <p className="text-fg-muted text-sm" style={{ marginTop: 4, marginBottom: 16 }}>
        Share a code so a visitor can join <strong>your room</strong> — they can browse, play, and
        add to the shared queue, but they don&apos;t touch your taste, playlists, or settings, and
        the session expires. Like a guest Wi-Fi password.
      </p>

      {fresh && <FreshCodeBanner code={fresh} onDismiss={() => setFresh(null)} />}

      <div style={{ display: "grid", gap: 10, marginBottom: 20 }}>
        <input
          value={label}
          onChange={(e) => setLabel(e.target.value)}
          placeholder="label (optional) — e.g. “party Saturday”"
          style={inputStyle}
        />
        <div style={{ display: "flex", gap: 10, flexWrap: "wrap" }}>
          <select
            value={expiryIdx}
            onChange={(e) => setExpiryIdx(Number(e.target.value))}
            style={{ ...inputStyle, flex: "1 1 160px" }}
          >
            {EXPIRY_OPTIONS.map((o, i) => (
              <option key={o.label} value={i}>
                expires: {o.label}
              </option>
            ))}
          </select>
          <input
            value={maxUses}
            onChange={(e) => setMaxUses(e.target.value.replace(/[^0-9]/g, ""))}
            placeholder="max uses (∞)"
            inputMode="numeric"
            style={{ ...inputStyle, flex: "1 1 120px" }}
          />
        </div>
        <SettingsButton onClick={() => create.mutate()}>
          <Plus size={14} strokeWidth={1.5} />
          {create.isPending ? "creating…" : "create code"}
        </SettingsButton>
        {create.isError && (
          <p className="text-sm" style={{ color: "var(--danger, #e05252)" }}>
            couldn&apos;t create a code — try again.
          </p>
        )}
      </div>

      {codes.isLoading ? (
        <p className="text-fg-muted text-sm">loading…</p>
      ) : codes.data && codes.data.length > 0 ? (
        <div>
          {codes.data.map((c) => (
            <GuestCodeRow
              key={c.id}
              code={c}
              onRevoke={() => revoke.mutate(c.id)}
              revoking={revoke.isPending && revoke.variables === c.id}
            />
          ))}
        </div>
      ) : (
        <p className="text-fg-muted text-sm">no guest codes yet.</p>
      )}
    </SettingsSection>
  );
}

function FreshCodeBanner({ code, onDismiss }: { code: CreatedGuestCode; onDismiss: () => void }) {
  const [copied, setCopied] = useState(false);
  return (
    <div
      style={{
        border: "1px solid var(--accent, #f0a020)",
        borderRadius: "var(--radius-2, 4px)",
        padding: 14,
        marginBottom: 18,
        background: "var(--bg-elevated)",
      }}
    >
      <p className="text-sm" style={{ marginTop: 0, marginBottom: 8 }}>
        Share this code now — it won&apos;t be shown again:
      </p>
      <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
        <code
          style={{
            fontSize: "1.4rem",
            letterSpacing: "0.12em",
            fontWeight: 600,
            fontVariantNumeric: "tabular-nums",
          }}
        >
          {code.code}
        </code>
        <SettingsButton
          onClick={() => {
            void navigator.clipboard?.writeText(code.code).then(() => {
              setCopied(true);
              setTimeout(() => setCopied(false), 1500);
            });
          }}
        >
          {copied ? <Check size={14} strokeWidth={1.5} /> : <Copy size={14} strokeWidth={1.5} />}
          {copied ? "copied" : "copy"}
        </SettingsButton>
        <SettingsButton onClick={onDismiss}>done</SettingsButton>
      </div>
    </div>
  );
}

function GuestCodeRow({
  code,
  onRevoke,
  revoking,
}: {
  code: GuestCode;
  onRevoke: () => void;
  revoking: boolean;
}) {
  const revoked = code.revoked_at_unix_ms != null;
  const expired =
    code.expires_at_unix_ms != null && code.expires_at_unix_ms <= Date.now();
  const dead = revoked || expired;
  const usesLabel =
    code.max_uses != null ? `${code.uses}/${code.max_uses} uses` : `${code.uses} uses`;
  const expiryLabel =
    code.expires_at_unix_ms == null
      ? "no expiry"
      : `expires ${new Date(code.expires_at_unix_ms).toLocaleString()}`;

  return (
    <div
      style={{
        display: "flex",
        justifyContent: "space-between",
        alignItems: "center",
        gap: 12,
        padding: "10px 0",
        borderBottom: "1px solid var(--border-subtle, var(--border))",
        opacity: dead ? 0.5 : 1,
      }}
    >
      <div style={{ minWidth: 0 }}>
        <div className="text-sm" style={{ fontWeight: 500 }}>
          {code.label || "guest code"}
          {revoked && <Badge>revoked</Badge>}
          {!revoked && expired && <Badge>expired</Badge>}
        </div>
        <div className="text-fg-muted text-sm">
          {usesLabel} · {expiryLabel}
        </div>
      </div>
      {!dead && (
        <SettingsButton danger onClick={onRevoke}>
          <Trash2 size={14} strokeWidth={1.5} />
          {revoking ? "…" : "revoke"}
        </SettingsButton>
      )}
    </div>
  );
}

function Badge({ children }: { children: React.ReactNode }) {
  return (
    <span
      className="text-sm"
      style={{
        marginLeft: 8,
        padding: "1px 6px",
        borderRadius: 999,
        border: "1px solid var(--border)",
        color: "var(--fg-muted)",
        fontSize: "0.72rem",
      }}
    >
      {children}
    </span>
  );
}

const inputStyle: React.CSSProperties = {
  width: "100%",
  padding: "8px 10px",
  borderRadius: "var(--radius-1, 2px)",
  background: "var(--bg-elevated)",
  color: "var(--fg)",
  border: "1px solid var(--border)",
  fontSize: "var(--text-base)",
  boxSizing: "border-box",
};
