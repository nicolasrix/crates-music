// /diagnostics — landing page. Lists each subpage with a short
// description of what lives there. Intentionally light: detail is one
// click away, and the sidebar already exposes the same set of links.

import { useState } from "react";

import { invalidateBrowseCache } from "../../api/diagnostics";
import { Layout } from "../../components/Layout";
import { Link } from "../../router";

interface Entry {
  to: string;
  label: string;
  blurb: string;
}

const ENTRIES: ReadonlyArray<Entry> = [
  {
    to: "/diagnostics/recommender",
    label: "recommender",
    blurb:
      "queue-fill ratio, shortfall reasons, admitted-similarity stats, most-recommended tracks.",
  },
  {
    to: "/diagnostics/latent",
    label: "latent space",
    blurb: "2-D UMAP scatter of all embedded tracks; hover to inspect, click to play.",
  },
  {
    to: "/diagnostics/ingest",
    label: "ingest",
    blurb: "embedder backlog: not-started / in-progress / done / failed.",
  },
  {
    to: "/diagnostics/tracing",
    label: "tracing",
    blurb: "span duration histogram and recent trace waterfalls.",
  },
  {
    to: "/diagnostics/rum",
    label: "client RUM",
    blurb: "browser web-vitals + custom marks emitted by the web app.",
  },
  {
    to: "/diagnostics/listening",
    label: "listening",
    blurb: "scrobble history with per-track repeat counts; sizes the MMR recency penalty.",
  },
];

export function DiagnosticsHome() {
  return (
    <Layout breadcrumb="diagnostics">
      <div className="section">
        <div className="section-head">
          <h2>diagnostics</h2>
        </div>

        <ul className="flex flex-col gap-3" style={{ listStyle: "none", padding: 0 }}>
          {ENTRIES.map((e) => (
            <li
              key={e.to}
              style={{
                border: "1px solid var(--border-subtle)",
                borderRadius: "var(--radius-2)",
                padding: "var(--space-3) var(--space-4)",
                background:
                  "color-mix(in oklab, var(--surface-1) 60%, transparent)",
              }}
            >
              <Link to={e.to} className="text-base font-medium">
                {e.label}
              </Link>
              <p className="text-fg-muted text-sm" style={{ marginTop: 4 }}>
                {e.blurb}
              </p>
            </li>
          ))}
        </ul>

        <CacheActions />
      </div>
    </Layout>
  );
}

type Status =
  | { kind: "idle" }
  | { kind: "running" }
  | { kind: "ok"; removed: number }
  | { kind: "error"; message: string };

function CacheActions() {
  const [status, setStatus] = useState<Status>({ kind: "idle" });

  async function onClick() {
    setStatus({ kind: "running" });
    try {
      const { removed } = await invalidateBrowseCache();
      setStatus({ kind: "ok", removed });
    } catch (err) {
      setStatus({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  }

  return (
    <div
      style={{
        marginTop: "var(--space-6)",
        border: "1px solid var(--border-subtle)",
        borderRadius: "var(--radius-2)",
        padding: "var(--space-3) var(--space-4)",
        background:
          "color-mix(in oklab, var(--surface-1) 60%, transparent)",
      }}
    >
      <h3 className="text-base font-medium">actions</h3>
      <p className="text-fg-muted text-sm" style={{ marginTop: 4 }}>
        flush the gateway's metadata cache so new content added in Navidrome
        shows up immediately. Cover-art entries are preserved.
      </p>
      <div className="flex items-center gap-3" style={{ marginTop: "var(--space-3)" }}>
        <button
          type="button"
          onClick={onClick}
          disabled={status.kind === "running"}
          style={{
            padding: "8px 14px",
            borderRadius: "var(--radius-2)",
            background: "var(--accent)",
            color: "var(--on-accent)",
            border: 0,
            cursor: status.kind === "running" ? "default" : "pointer",
            opacity: status.kind === "running" ? 0.5 : 1,
            fontSize: "var(--text-sm)",
            fontWeight: 500,
          }}
        >
          {status.kind === "running" ? "refreshing…" : "refresh metadata cache"}
        </button>
        {status.kind === "ok" && (
          <span className="text-fg-muted text-sm">
            cleared {status.removed} {status.removed === 1 ? "entry" : "entries"}
          </span>
        )}
        {status.kind === "error" && (
          <span className="text-sm" style={{ color: "var(--danger)" }}>
            {status.message}
          </span>
        )}
      </div>
    </div>
  );
}
