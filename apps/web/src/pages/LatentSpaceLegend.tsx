// Shared legends for the latent-space scatter. Lifted out of
// LatentSpace.tsx so the lazy 3-D scene (LatentSpace3D) can reuse them
// without dragging the entire 2-D page into its chunk.
//
// The genre legend can be either interactive (with toggle handler, as
// used by the 2-D canvas where hiding a bucket hides its dots) or
// display-only (3-D view, where hiding would require re-uploading the
// colour buffer — not worth the complexity yet). `onToggle === undefined`
// flips it into display-only mode.

import { GenreBucket, viridis } from "./latentSpace";

export type LegendColorMode = "genre" | "pc1" | "pc2" | "pc3" | "pc4";

export function Legend({
  buckets,
  hidden,
  onToggle,
}: {
  buckets: readonly GenreBucket[];
  /** Required when `onToggle` is set; ignored otherwise. */
  hidden?: ReadonlySet<string>;
  /** Omit to make the legend display-only (no click, no hidden state). */
  onToggle?: (label: string) => void;
}) {
  if (buckets.length === 0) return null;
  const interactive = !!onToggle;
  return (
    <ul
      style={{
        listStyle: "none",
        padding: 0,
        margin: 0,
        minWidth: 160,
        fontSize: "0.85em",
      }}
    >
      {buckets.map((b) => {
        const isHidden = !!hidden?.has(b.label);
        const rowProps = interactive
          ? {
              role: "button" as const,
              tabIndex: 0,
              onClick: () => onToggle!(b.label),
              onKeyDown: (e: React.KeyboardEvent) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  onToggle!(b.label);
                }
              },
            }
          : {};
        return (
          <li
            key={b.label}
            // Plain <li> with role=button (only when interactive) gives
            // keyboard users the right affordance without breaking the
            // layout. Without onToggle, the row is a static legend entry.
            {...rowProps}
            style={{
              display: "flex",
              alignItems: "center",
              gap: "var(--space-2, 8px)",
              padding: "2px 4px",
              borderRadius: "var(--radius-1, 2px)",
              cursor: interactive ? "pointer" : "default",
              opacity: isHidden ? 0.4 : 1,
              userSelect: "none",
            }}
          >
            <span
              aria-hidden="true"
              style={{
                width: 10,
                height: 10,
                borderRadius: "50%",
                background: b.color,
                flexShrink: 0,
                // Strike-through when hidden gives a visual cue beyond opacity.
                outline: isHidden ? "1px solid var(--muted)" : "none",
              }}
            />
            <span
              style={{
                textDecoration: isHidden ? "line-through" : "none",
                color: isHidden ? "var(--muted)" : "inherit",
                flex: 1,
                minWidth: 0,
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
              title={b.label}
            >
              {b.label}
            </span>
            <span style={{ color: "var(--muted)", fontVariantNumeric: "tabular-nums" }}>
              {b.count}
            </span>
          </li>
        );
      })}
    </ul>
  );
}

export function PcGradientLegend({
  mode,
  range,
}: {
  mode: LegendColorMode;
  range: { min: number; max: number } | null;
}) {
  if (mode === "genre") return null;
  // Pretty-print: PC modes uppercase to "PC1".
  const label = mode.toUpperCase();
  if (!range) {
    return (
      <div style={{ minWidth: 160, fontSize: "0.85em" }}>
        <div style={{ color: "var(--muted)" }}>
          {label} not yet populated for this projection. Re-run the reducer
          to compute it.
        </div>
      </div>
    );
  }
  // Build the CSS gradient from the same 5 stops the viridis() helper
  // uses, so the legend matches the canvas exactly.
  const stops = [0, 0.25, 0.5, 0.75, 1].map((t) => viridis(t)).join(", ");
  return (
    <div style={{ minWidth: 160, fontSize: "0.85em" }}>
      <div style={{ color: "var(--muted)", marginBottom: 4 }}>
        {label} (continuous)
      </div>
      <div
        aria-hidden="true"
        style={{
          height: 12,
          background: `linear-gradient(to right, ${stops})`,
          borderRadius: "var(--radius-1, 2px)",
          marginBottom: 4,
        }}
      />
      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          color: "var(--muted)",
          fontVariantNumeric: "tabular-nums",
        }}
      >
        <span>{range.min.toFixed(2)}</span>
        <span>{range.max.toFixed(2)}</span>
      </div>
    </div>
  );
}
