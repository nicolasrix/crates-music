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
    if (!code) {
      setError("missing ?code in OAuth callback");
      return;
    }
    completeLogin(code)
      // Hard reload so AuthProvider re-reads tokens from localStorage.
      .then(() => location.assign("/"))
      .catch((e: unknown) => setError(String(e)));
  }, []);
  return (
    <div className="min-h-screen flex items-center justify-center">
      {error ? (
        <div className="max-w-md text-center">
          <h1 className="text-xl font-semibold mb-2">sign-in failed</h1>
          <p className="text-stone-400">{error}</p>
          <button
            onClick={() => location.assign("/")}
            className="mt-4 px-3 py-1 rounded bg-stone-800 hover:bg-stone-700"
          >
            back
          </button>
        </div>
      ) : (
        <p className="text-stone-400">finishing sign-in…</p>
      )}
    </div>
  );
}
