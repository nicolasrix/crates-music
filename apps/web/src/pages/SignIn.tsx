import { useAuth } from "../auth/AuthContext";
import { BrandMark } from "../components/BrandMark";

export function SignIn() {
  const { login, loading } = useAuth();
  return (
    <div
      className="flex items-center justify-center"
      style={{ minHeight: "100vh", padding: "var(--space-5)" }}
    >
      <div className="text-center" style={{ maxWidth: 360 }}>
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
          disabled={loading}
          style={{
            padding: "10px 18px",
            borderRadius: "var(--radius-2)",
            background: "var(--accent)",
            color: "var(--on-accent)",
            border: 0,
            cursor: loading ? "default" : "pointer",
            opacity: loading ? 0.5 : 1,
            fontWeight: 500,
            fontSize: "var(--text-base)",
          }}
        >
          {loading ? "checking session…" : "sign in"}
        </button>
      </div>
    </div>
  );
}
