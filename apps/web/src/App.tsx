import { useEffect } from "react";

import { useAuth } from "./auth/AuthContext";
import { Album } from "./pages/Album";
import { Albums } from "./pages/Albums";
import { Callback } from "./pages/Callback";
import { Diagnostics } from "./pages/Diagnostics";
import { SignIn } from "./pages/SignIn";
import { PlayerBar } from "./player/PlayerBar";
import { PlayerProvider } from "./player/PlayerContext";
import { useRoute } from "./router";
import { initRum } from "./rum";
import { SyncProvider } from "./sync/SyncContext";

export function App() {
  const { path } = useRoute();
  const { tokens } = useAuth();

  // Boot RUM once we have tokens. The emitter is a no-op until then
  // (uploadEvents bails out without auth), but installing the
  // web-vitals subscribers earlier captures the LCP/FCP that fire
  // *before* sign-in, which is when load-perf actually matters.
  useEffect(() => {
    if (tokens) initRum();
  }, [tokens]);

  if (path === "/oauth/callback") return <Callback />;

  if (!tokens) return <SignIn />;

  return (
    <SyncProvider>
      <PlayerProvider>
        <Routed path={path} />
        <PlayerBar />
      </PlayerProvider>
    </SyncProvider>
  );
}

function Routed({ path }: { path: string }) {
  const m = path.match(/^\/albums\/([^/]+)$/);
  if (m && m[1]) return <Album id={m[1]} />;
  if (path === "/diagnostics") return <Diagnostics />;
  return <Albums />;
}
