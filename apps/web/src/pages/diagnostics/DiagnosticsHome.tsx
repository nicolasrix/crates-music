// /diagnostics — landing page. Lists each subpage with a short
// description of what lives there. Intentionally light: detail is one
// click away, and the sidebar already exposes the same set of links.

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
      </div>
    </Layout>
  );
}
