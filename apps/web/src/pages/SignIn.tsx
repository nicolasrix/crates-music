import { useAuth } from "../auth/AuthContext";

export function SignIn() {
  const { login, loading } = useAuth();
  return (
    <div className="min-h-screen flex items-center justify-center">
      <div className="max-w-sm text-center">
        <h1 className="text-3xl font-semibold mb-2">music</h1>
        <p className="text-stone-400 mb-8">self-hosted player for your Navidrome</p>
        <button
          onClick={login}
          disabled={loading}
          className="px-4 py-2 rounded bg-stone-200 text-stone-900 hover:bg-white disabled:opacity-50"
        >
          {loading ? "checking session…" : "sign in"}
        </button>
      </div>
    </div>
  );
}
