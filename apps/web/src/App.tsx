import { useAuth } from "./auth/AuthContext";
import { Album } from "./pages/Album";
import { Albums } from "./pages/Albums";
import { Callback } from "./pages/Callback";
import { SignIn } from "./pages/SignIn";
import { PlayerBar } from "./player/PlayerBar";
import { PlayerProvider } from "./player/PlayerContext";
import { useRoute } from "./router";

export function App() {
  const { path } = useRoute();
  const { tokens } = useAuth();

  if (path === "/oauth/callback") return <Callback />;

  if (!tokens) return <SignIn />;

  return (
    <PlayerProvider>
      <Routed path={path} />
      <PlayerBar />
    </PlayerProvider>
  );
}

function Routed({ path }: { path: string }) {
  const m = path.match(/^\/albums\/([^/]+)$/);
  if (m && m[1]) return <Album id={m[1]} />;
  return <Albums />;
}
