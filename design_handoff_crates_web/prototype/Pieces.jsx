// Small primitives — Lucide-traced icons and atomic UI pieces.
// All consume CSS variables from ../../colors_and_type.css.

const { useState, useEffect } = React;

// Lucide-traced (https://lucide.dev — MIT). Stroke 1.5, currentColor.
const Icon = ({ name, size = 18, fill = false, style }) => {
  const s = { width: size, height: size, stroke: "currentColor", strokeWidth: 1.5, fill: fill ? "currentColor" : "none", strokeLinecap: "round", strokeLinejoin: "round", flexShrink: 0, ...style };
  switch (name) {
    case "play":      return <svg viewBox="0 0 24 24" style={{...s, stroke: "none", fill: "currentColor"}}><polygon points="6 3 20 12 6 21 6 3"/></svg>;
    case "pause":     return <svg viewBox="0 0 24 24" style={{...s, stroke: "none", fill: "currentColor"}}><rect x="6" y="4" width="4" height="16"/><rect x="14" y="4" width="4" height="16"/></svg>;
    case "skip-fwd":  return <svg viewBox="0 0 24 24" style={s}><polygon points="5 4 15 12 5 20 5 4"/><line x1="19" y1="5" x2="19" y2="19"/></svg>;
    case "skip-back": return <svg viewBox="0 0 24 24" style={s}><polygon points="19 20 9 12 19 4 19 20"/><line x1="5" y1="19" x2="5" y2="5"/></svg>;
    case "repeat":    return <svg viewBox="0 0 24 24" style={s}><path d="m17 2 4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="m7 22-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/></svg>;
    case "search":    return <svg viewBox="0 0 24 24" style={s}><circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/></svg>;
    case "disc":      return <svg viewBox="0 0 24 24" style={s}><circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="3"/></svg>;
    case "list":      return <svg viewBox="0 0 24 24" style={s}><path d="M21 15V6"/><path d="M18.5 18a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0Z"/><path d="M12 12H3"/><path d="M16 6H3"/><path d="M12 18H3"/></svg>;
    case "user":      return <svg viewBox="0 0 24 24" style={s}><circle cx="12" cy="8" r="4"/><path d="M6 21v-2a4 4 0 0 1 4-4h4a4 4 0 0 1 4 4v2"/></svg>;
    case "plus":      return <svg viewBox="0 0 24 24" style={s}><path d="M12 5v14"/><path d="M5 12h14"/></svg>;
    case "more":      return <svg viewBox="0 0 24 24" style={s}><circle cx="5" cy="12" r="1.5" fill="currentColor"/><circle cx="12" cy="12" r="1.5" fill="currentColor"/><circle cx="19" cy="12" r="1.5" fill="currentColor"/></svg>;
    case "chevron-l": return <svg viewBox="0 0 24 24" style={s}><polyline points="15 18 9 12 15 6"/></svg>;
    case "chevron-r": return <svg viewBox="0 0 24 24" style={s}><polyline points="9 6 15 12 9 18"/></svg>;
    case "library":   return <svg viewBox="0 0 24 24" style={s}><path d="M3 6v14"/><path d="M8 6v14"/><path d="M13 4v18l4-2 4 2V4Z"/></svg>;
    case "auto":      return <svg viewBox="0 0 24 24" style={s}><path d="M21 12a9 9 0 1 1-9-9"/><path d="M21 4v8h-8"/><circle cx="12" cy="12" r="2" fill="currentColor"/></svg>;
    case "diagnostics": return <svg viewBox="0 0 24 24" style={s}><path d="M3 12h4l2-7 4 14 2-7h6"/></svg>;
    case "settings":  return <svg viewBox="0 0 24 24" style={s}><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1Z"/></svg>;
    default:          return null;
  }
};

const Cover = ({ src, size = 160, radius = "var(--radius-3)" }) => (
  <div style={{ background: src, width: size, height: size, borderRadius: radius, flexShrink: 0 }}/>
);

const IconBtn = ({ children, onClick, title, on, className = "", style }) => (
  <button className={`icon-btn ${on ? "on" : ""} ${className}`} onClick={onClick} title={title} style={style}>
    {children}
  </button>
);

const Pill = ({ tone = "info", children }) => {
  const map = {
    good:  { bg: "color-mix(in oklab, var(--success) 18%, transparent)", color: "var(--success-200)", dot: "var(--success)" },
    "needs-improvement": { bg: "color-mix(in oklab, var(--warning) 18%, transparent)", color: "var(--warning-200)", dot: "var(--warning)" },
    poor:  { bg: "color-mix(in oklab, var(--danger) 18%, transparent)", color: "var(--danger-200)", dot: "var(--danger)" },
    info:  { bg: "color-mix(in oklab, var(--info) 18%, transparent)", color: "var(--info-200)", dot: "var(--info)" },
  };
  const t = map[tone] || map.info;
  return <span className="pill" style={{ background: t.bg, color: t.color }}><span className="dot" style={{background: t.dot}}/>{children}</span>;
};

const Tag = ({ children, tone }) => (
  <span style={{
    display:"inline-flex", alignItems:"center", padding:"3px 8px",
    borderRadius:"var(--radius-1)",
    background: tone === "accent" ? "color-mix(in oklab, var(--accent) 14%, transparent)"
              : tone === "success" ? "color-mix(in oklab, var(--success) 12%, transparent)"
              : "var(--surface-2)",
    color: tone === "accent" ? "var(--accent)"
         : tone === "success" ? "var(--success-200)"
         : "var(--fg-muted)",
    fontFamily: "var(--font-mono)", fontSize: 11
  }}>{children}</span>
);

const Search = ({ value, onChange, placeholder = "search albums, artists, tracks…" }) => (
  <div style={{ position: "relative", width: "100%" }}>
    <span style={{ position:"absolute", left: 10, top: "50%", transform: "translateY(-50%)", color: "var(--fg-faint)" }}>
      <Icon name="search" size={14}/>
    </span>
    <input
      value={value || ""}
      onChange={e => onChange?.(e.target.value)}
      placeholder={placeholder}
      style={{
        background: "var(--surface-2)", color: "var(--fg)",
        border: "1px solid var(--border-subtle)",
        padding: "8px 12px 8px 32px", borderRadius: "var(--radius-2)",
        font: "var(--type-meta)", outline: "none", width: "100%"
      }}
    />
  </div>
);

const fmtDuration = (s) => {
  const m = Math.floor(s / 60);
  const r = Math.floor(s % 60);
  return `${m}:${String(r).padStart(2, "0")}`;
};
const fmtMs = (n) => n == null ? "—" : n < 1 ? "<1ms" : n < 1000 ? `${Math.round(n)}ms` : `${(n / 1000).toFixed(2)}s`;

window.UI = { Icon, Cover, IconBtn, Pill, Tag, Search, fmtDuration, fmtMs };
