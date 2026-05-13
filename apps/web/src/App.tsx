import { useEffect } from "react";

import { useAuth } from "./auth/AuthContext";
import { ArtworkProvider } from "./components/ArtworkPalette";
import { Album } from "./pages/Album";
import { Albums } from "./pages/Albums";
import { Artist } from "./pages/Artist";
import { Artists } from "./pages/Artists";
import { Callback } from "./pages/Callback";
import { DiagnosticsHome } from "./pages/diagnostics/DiagnosticsHome";
import { Ingest } from "./pages/diagnostics/Ingest";
import { Listening } from "./pages/diagnostics/Listening";
import { Recommender } from "./pages/diagnostics/Recommender";
import { Rum } from "./pages/diagnostics/Rum";
import { Tracing } from "./pages/diagnostics/Tracing";
import { Home } from "./pages/Home";
import { LatentSpace } from "./pages/LatentSpace";
import { Playlist } from "./pages/Playlist";
import { Queue } from "./pages/Queue";
import { Search } from "./pages/Search";
import { SearchBucket } from "./pages/SearchBucket";
import { SignIn } from "./pages/SignIn";
import { Station } from "./pages/Station";
import { Tracks } from "./pages/Tracks";
import { AutoplayProvider } from "./player/AutoplayContext";
import { PlayerBar } from "./player/PlayerBar";
import { PlayerProvider } from "./player/PlayerContext";
import { modeFromSlug } from "./pages/listMode";
import { useRoute } from "./router";
import { initRum } from "./rum";
import { SyncProvider } from "./sync/SyncContext";

export function App() {
  const { path } = useRoute();
  const { tokens } = useAuth();

  useEffect(() => {
    if (tokens) initRum();
  }, [tokens]);

  if (path === "/oauth/callback") return <Callback />;
  if (!tokens) return <SignIn />;

  return (
    <SyncProvider>
      <PlayerProvider>
        <AutoplayProvider>
          <ArtworkProvider>
            <div className="shell">
              <Routed path={path} />
              <PlayerBar />
            </div>
          </ArtworkProvider>
        </AutoplayProvider>
      </PlayerProvider>
    </SyncProvider>
  );
}

function Routed({ path }: { path: string }) {
  // Album detail. Detail ids overlap with mode slugs in the URL space, so
  // we disambiguate: /albums/recent|most-played|random go to the listing,
  // anything else with one segment is treated as an album id.
  const albumModeSlugs = new Set(["recent", "most-played", "random"]);

  let m = path.match(/^\/albums\/([^/]+)$/);
  if (m && m[1] && !albumModeSlugs.has(m[1])) return <Album id={m[1]} />;

  m = path.match(/^\/albums(?:\/(recent|most-played|random))?$/);
  if (m) return <Albums mode={modeFromSlug(m[1])} />;

  m = path.match(/^\/artists\/([^/]+)$/);
  if (m && m[1] && !albumModeSlugs.has(m[1])) return <Artist id={m[1]} />;

  m = path.match(/^\/artists(?:\/(recent|most-played|random))?$/);
  if (m) return <Artists mode={modeFromSlug(m[1])} />;

  m = path.match(/^\/tracks(?:\/(recent|most-played|random))?$/);
  if (m) return <Tracks mode={modeFromSlug(m[1])} />;

  // /playlists/:id → playlist detail
  m = path.match(/^\/playlists\/([^/]+)$/);
  if (m && m[1]) return <Playlist id={m[1]} />;

  if (path === "/search") return <Search />;
  if (path === "/search/artists") return <SearchBucket bucket="artists" />;
  if (path === "/search/albums") return <SearchBucket bucket="albums" />;
  if (path === "/search/tracks") return <SearchBucket bucket="tracks" />;
  if (path === "/queue") return <Queue />;
  if (path === "/station") return <Station />;
  if (path === "/diagnostics") return <DiagnosticsHome />;
  if (path === "/diagnostics/recommender") return <Recommender />;
  if (path === "/diagnostics/latent") return <LatentSpace />;
  if (path === "/diagnostics/ingest") return <Ingest />;
  if (path === "/diagnostics/tracing") return <Tracing />;
  if (path === "/diagnostics/rum") return <Rum />;
  if (path === "/diagnostics/listening") return <Listening />;
  return <Home />;
}
