// Sticky 56px topbar inside <main>. Back/forward arrows drive history; the
// breadcrumb is purely decorative for now. The sign-out and settings live on
// the right.

import { ChevronLeft, ChevronRight, CloudOff, Settings, LogOut } from "lucide-react";
import { useAuth } from "../auth/AuthContext";
import { useOnline } from "../cache/useOnline";

export function Topbar({ breadcrumb }: { breadcrumb?: string | undefined }) {
  const { logout } = useAuth();
  const online = useOnline();
  return (
    <header className="topbar">
      <div className="flex gap-1">
        <button
          className="arrow"
          onClick={() => history.back()}
          aria-label="back"
          title="back"
        >
          <ChevronLeft size={16} strokeWidth={1.5} />
        </button>
        <button
          className="arrow"
          onClick={() => history.forward()}
          aria-label="forward"
          title="forward"
        >
          <ChevronRight size={16} strokeWidth={1.5} />
        </button>
      </div>
      <div className="breadcrumb">{breadcrumb ?? ""}</div>
      <div className="ml-auto flex items-center gap-1">
        {!online && (
          <span
            className="text-sm"
            title="You're offline — only downloaded and recently-played tracks will play."
            style={{
              display: "inline-flex",
              alignItems: "center",
              gap: 6,
              padding: "3px 8px",
              marginRight: 4,
              borderRadius: "var(--radius-1, 2px)",
              background: "var(--bg-elevated)",
              border: "1px solid var(--border)",
              color: "var(--fg-muted)",
            }}
          >
            <CloudOff size={14} strokeWidth={1.5} />
            offline
          </span>
        )}
        <button
          className="arrow"
          aria-label="settings"
          title="settings"
          onClick={() => {
            /* settings not implemented yet */
          }}
        >
          <Settings size={16} strokeWidth={1.5} />
        </button>
        <button
          className="arrow"
          aria-label="sign out"
          title="sign out"
          onClick={() => void logout()}
        >
          <LogOut size={16} strokeWidth={1.5} />
        </button>
      </div>
    </header>
  );
}
