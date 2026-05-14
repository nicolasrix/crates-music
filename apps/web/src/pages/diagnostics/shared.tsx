// Shared chrome used by every /diagnostics/* subpage. Lives here so the
// individual pages stay focused on their own data.

import { ReactNode } from "react";

export const REFRESH_MS = 5_000;

export function DiagSection({
  title,
  children,
}: {
  title: string;
  children: ReactNode;
}) {
  return (
    <section style={{ marginBottom: "var(--space-7)" }}>
      <h3 className="text-lg font-medium mb-3">{title}</h3>
      {children}
    </section>
  );
}

export function ErrorLine({ error }: { error: unknown }) {
  return <p className="text-danger text-sm">error: {(error as Error).message}</p>;
}

export function fmtRecentTime(unixMs: number): string {
  return new Date(unixMs).toLocaleTimeString();
}
