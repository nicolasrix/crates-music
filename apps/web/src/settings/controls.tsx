// Shared, declarative settings controls. Before this, Settings.tsx
// re-implemented the same flex/label/range/help markup four times with
// inline styles; every section now composes these instead.

import { ReactNode } from "react";

const accent = "var(--accent, #f0a020)";
const fieldStyle = {
  background: "var(--bg-elevated)",
  border: "1px solid var(--border)",
  borderRadius: "var(--radius-1, 2px)",
  color: "var(--fg)",
} as const;

/** A top-level settings card: `.section` with an h2 head + optional action
 *  (e.g. a scoped "reset" button) on the right. */
export function SettingsSection({
  icon,
  title,
  action,
  children,
}: {
  icon?: ReactNode;
  title: string;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="section" style={{ marginTop: 24 }}>
      <div className="section-head">
        <h2 style={{ margin: 0 }}>
          {icon && (
            <span style={{ verticalAlign: "-3px", marginRight: 8, display: "inline-flex" }}>
              {icon}
            </span>
          )}
          {title}
        </h2>
        {action}
      </div>
      {children}
    </div>
  );
}

/** An h3 subgroup inside a section (e.g. autoplay's Boundary / Direction). */
export function SettingsSubgroup({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div style={{ marginTop: 20 }}>
      <h3 style={{ margin: "0 0 12px", fontSize: "0.95rem" }}>{title}</h3>
      {children}
    </div>
  );
}

/** Shared label + value header row used by sliders and selects. */
function RowHead({ htmlFor, label, value }: { htmlFor?: string; label: string; value?: ReactNode }) {
  return (
    <div
      style={{
        display: "flex",
        justifyContent: "space-between",
        alignItems: "baseline",
        marginBottom: 2,
      }}
    >
      <label htmlFor={htmlFor} style={{ fontWeight: 500 }}>
        {label}
      </label>
      {value !== undefined && (
        <span className="text-sm" style={{ fontVariantNumeric: "tabular-nums", color: "var(--fg)" }}>
          {value}
        </span>
      )}
    </div>
  );
}

export function SliderRow({
  id,
  label,
  displayValue,
  min,
  max,
  step,
  value,
  lowHint,
  highHint,
  help,
  onChange,
}: {
  id: string;
  label: string;
  displayValue: ReactNode;
  min: number;
  max: number;
  step: number;
  value: number;
  lowHint?: string;
  highHint?: string;
  help?: string;
  onChange: (v: number) => void;
}) {
  return (
    <div style={{ marginBottom: 18 }}>
      <RowHead htmlFor={id} label={label} value={displayValue} />
      <input
        id={id}
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        style={{ width: "100%", accentColor: accent }}
      />
      {(lowHint || highHint) && (
        <div
          className="text-fg-muted text-sm"
          style={{ display: "flex", justifyContent: "space-between", marginTop: 2 }}
        >
          <span>{lowHint}</span>
          <span>{highHint}</span>
        </div>
      )}
      {help && (
        <p className="text-fg-muted text-sm" style={{ marginTop: 6 }}>
          {help}
        </p>
      )}
    </div>
  );
}

export function ToggleRow({
  label,
  checked,
  help,
  onChange,
}: {
  label: string;
  checked: boolean;
  help?: string;
  onChange: (v: boolean) => void;
}) {
  return (
    <div style={{ marginBottom: help ? 8 : 18 }}>
      <label style={{ display: "flex", alignItems: "center", gap: 10 }}>
        <input
          type="checkbox"
          checked={checked}
          onChange={(e) => onChange(e.target.checked)}
          style={{ accentColor: accent }}
        />
        <span style={{ fontWeight: 500 }}>{label}</span>
      </label>
      {help && (
        <p className="text-fg-muted text-sm" style={{ marginTop: 6 }}>
          {help}
        </p>
      )}
    </div>
  );
}

export function SelectRow<T extends string>({
  id,
  label,
  value,
  options,
  help,
  onChange,
}: {
  id: string;
  label: string;
  value: T;
  options: readonly { value: T; label: string }[];
  help?: string;
  onChange: (v: T) => void;
}) {
  return (
    <div style={{ marginBottom: 18 }}>
      <RowHead
        htmlFor={id}
        label={label}
        value={
          <select
            id={id}
            value={value}
            onChange={(e) => onChange(e.target.value as T)}
            style={{ ...fieldStyle, padding: "4px 8px" }}
          >
            {options.map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
          </select>
        }
      />
      {help && (
        <p className="text-fg-muted text-sm" style={{ marginTop: 6 }}>
          {help}
        </p>
      )}
    </div>
  );
}

/** Read-only key/value line for the Account / About sections. */
export function InfoRow({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div
      style={{
        display: "flex",
        justifyContent: "space-between",
        gap: 16,
        padding: "6px 0",
        borderBottom: "1px solid var(--border-subtle, var(--border))",
      }}
    >
      <span className="text-fg-muted text-sm">{label}</span>
      <span
        className="text-sm"
        style={{ color: "var(--fg)", textAlign: "right", wordBreak: "break-all" }}
      >
        {value}
      </span>
    </div>
  );
}

/** Small bordered button used for section actions (reset, sign out, install). */
export function SettingsButton({
  onClick,
  children,
  danger,
}: {
  onClick: () => void;
  children: ReactNode;
  danger?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="text-sm"
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: 6,
        padding: "6px 10px",
        ...fieldStyle,
        color: danger ? "var(--danger, #e05252)" : "var(--fg)",
        cursor: "pointer",
      }}
    >
      {children}
    </button>
  );
}
