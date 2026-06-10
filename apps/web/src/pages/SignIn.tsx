import { useState } from "react";
import { useAuth } from "../auth/AuthContext";
import { BrandMark } from "../components/BrandMark";

export function SignIn() {
  const { login, loading, joinGuest } = useAuth();
  const [showGuest, setShowGuest] = useState(false);
  const [code, setCode] = useState("");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submitGuest(e: React.FormEvent) {
    e.preventDefault();
    if (!code.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await joinGuest(code, name);
      // joinGuest flips the auth state; the app re-renders into the host's
      // room. No explicit navigation needed.
    } catch (err) {
      setError(err instanceof Error ? err.message : "could not join");
      setBusy(false);
    }
  }

  return (
    <div
      className="flex items-center justify-center"
      style={{ minHeight: "100vh", padding: "var(--space-5)" }}
    >
      <div className="text-center" style={{ maxWidth: 360, width: "100%" }}>
        <div className="flex justify-center mb-4">
          <BrandMark size={56} />
        </div>
        <h1
          className="t-display-sm"
          style={{ marginBottom: "var(--space-2)", fontWeight: 500 }}
        >
          crates
        </h1>
        <p className="text-fg-muted text-sm" style={{ marginBottom: "var(--space-7)" }}>
          self-hosted player for your Navidrome.
        </p>
        <button
          onClick={login}
          disabled={loading || busy}
          style={{
            padding: "10px 18px",
            borderRadius: "var(--radius-2)",
            background: "var(--accent)",
            color: "var(--on-accent)",
            border: 0,
            cursor: loading ? "default" : "pointer",
            opacity: loading || busy ? 0.5 : 1,
            fontWeight: 500,
            fontSize: "var(--text-base)",
          }}
        >
          {loading ? "checking session…" : "sign in"}
        </button>

        <div style={{ marginTop: "var(--space-6)" }}>
          {!showGuest ? (
            <button
              onClick={() => setShowGuest(true)}
              className="text-fg-muted text-sm"
              style={{ background: "none", border: 0, cursor: "pointer", textDecoration: "underline" }}
            >
              join with a guest code
            </button>
          ) : (
            <form onSubmit={submitGuest} style={{ textAlign: "left" }}>
              <label
                className="text-fg-muted text-sm"
                style={{ display: "block", marginBottom: "var(--space-2)" }}
              >
                guest code
              </label>
              <input
                value={code}
                onChange={(e) => setCode(e.target.value.toUpperCase())}
                placeholder="XXXX-XXXX"
                autoCapitalize="characters"
                autoCorrect="off"
                spellCheck={false}
                style={inputStyle}
              />
              <input
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="your name (optional)"
                style={{ ...inputStyle, marginTop: "var(--space-2)" }}
              />
              {error && (
                <p className="text-sm" style={{ color: "var(--danger, #c0392b)", marginTop: "var(--space-2)" }}>
                  {error}
                </p>
              )}
              <button
                type="submit"
                disabled={busy || !code.trim()}
                style={{
                  marginTop: "var(--space-3)",
                  padding: "8px 16px",
                  borderRadius: "var(--radius-2)",
                  background: "var(--surface-2, #2a2a2a)",
                  color: "var(--fg)",
                  border: "1px solid var(--border, #444)",
                  cursor: busy || !code.trim() ? "default" : "pointer",
                  opacity: busy || !code.trim() ? 0.5 : 1,
                  fontWeight: 500,
                  width: "100%",
                }}
              >
                {busy ? "joining…" : "join"}
              </button>
            </form>
          )}
        </div>
      </div>
    </div>
  );
}

const inputStyle: React.CSSProperties = {
  width: "100%",
  padding: "10px 12px",
  borderRadius: "var(--radius-2)",
  background: "var(--surface-1, #1a1a1a)",
  color: "var(--fg)",
  border: "1px solid var(--border, #444)",
  fontSize: "var(--text-base)",
  boxSizing: "border-box",
};
