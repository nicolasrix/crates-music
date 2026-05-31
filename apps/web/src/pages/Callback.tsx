import { useEffect, useRef, useState } from "react";
import { completeLogin } from "../auth/oauth";

export function Callback() {
  const [error, setError] = useState<string | null>(null);
  const ranRef = useRef(false);
  useEffect(() => {
    if (ranRef.current) return;
    ranRef.current = true;
    const params = new URLSearchParams(location.search);
    const code = params.get("code");
    const state = params.get("state");
    if (!code) {
      setError("missing ?code in OAuth callback");
      return;
    }
    completeLogin(code, state)
      .then(() => location.assign("/"))
      .catch((e: unknown) => setError(String(e)));
  }, []);
  return (
    <div
      className="flex items-center justify-center"
      style={{ minHeight: "100vh", padding: "var(--space-5)" }}
    >
      {error ? (
        <div className="text-center" style={{ maxWidth: 480 }}>
          <h1 className="text-xl font-medium mb-2">sign-in failed</h1>
          <p className="text-fg-muted text-sm">{error}</p>
          <button
            onClick={() => location.assign("/")}
            style={{
              marginTop: "var(--space-4)",
              padding: "8px 14px",
              borderRadius: "var(--radius-2)",
              background: "var(--surface-2)",
              color: "var(--fg)",
              border: 0,
              cursor: "pointer",
            }}
          >
            back
          </button>
        </div>
      ) : (
        <p className="text-fg-muted text-sm">finishing sign-in…</p>
      )}
    </div>
  );
}
