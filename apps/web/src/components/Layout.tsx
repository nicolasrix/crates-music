import { ReactNode } from "react";
import { useAuth } from "../auth/AuthContext";
import { Link } from "../router";

export function Layout({ children }: { children: ReactNode }) {
  const { logout } = useAuth();
  return (
    <div className="min-h-screen pb-24">
      <header className="sticky top-0 z-10 border-b border-stone-800 bg-stone-950/80 backdrop-blur">
        <div className="max-w-5xl mx-auto px-4 py-3 flex items-center justify-between">
          <Link to="/" className="text-lg font-medium">
            music
          </Link>
          <button
            onClick={() => void logout()}
            className="text-sm text-stone-400 hover:text-stone-100"
          >
            sign out
          </button>
        </div>
      </header>
      <main className="max-w-5xl mx-auto px-4 py-6">{children}</main>
    </div>
  );
}
