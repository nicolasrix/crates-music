// The disc-mark from design_handoff_crates_web/assets/logo-mark.svg, inlined so
// it picks up `currentColor` and CSS-var fills without an HTTP fetch.
export function BrandMark({ size = 28 }: { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 64 64"
      aria-label="crates"
      role="img"
    >
      <circle cx="32" cy="32" r="30" fill="var(--surface-1)" />
      <circle cx="32" cy="32" r="22" fill="none" stroke="rgba(255,255,255,0.05)" strokeWidth="0.6" />
      <circle cx="32" cy="32" r="16" fill="none" stroke="rgba(255,255,255,0.05)" strokeWidth="0.6" />
      <circle cx="32" cy="32" r="11" fill="var(--accent)" />
      <circle cx="32" cy="32" r="1.4" fill="var(--surface-0)" />
    </svg>
  );
}
