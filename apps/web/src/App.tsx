import { ComponentType, useEffect } from "react";

import { useAuth } from "./auth/AuthContext";
import { AudioCacheProvider } from "./cache/AudioCacheContext";
import { ArtworkProvider } from "./components/ArtworkPalette";
import { Album } from "./pages/Album";
import { Albums } from "./pages/Albums";
import { Artist } from "./pages/Artist";
import { Artists } from "./pages/Artists";
import { Callback } from "./pages/Callback";
import { Downloads } from "./pages/Downloads";
import { Ingest } from "./pages/diagnostics/Ingest";
import { Listening } from "./pages/diagnostics/Listening";
import { Recommender } from "./pages/diagnostics/Recommender";
import { Rum } from "./pages/diagnostics/Rum";
import { Tracing } from "./pages/diagnostics/Tracing";
import { Home } from "./pages/Home";
import { LatentSpace } from "./pages/LatentSpace";
import { LikedSongs } from "./pages/LikedSongs";
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
import { useIsAdmin } from "./auth/useWhoami";
import { DEFAULT_PANEL, panelVisible } from "./settings/nav";
import { AboutPanel } from "./settings/panels/AboutPanel";
import { AccountPanel } from "./settings/panels/AccountPanel";
import { AppearancePanel } from "./settings/panels/AppearancePanel";
import { AutoplayPanel } from "./settings/panels/AutoplayPanel";
import { GuestsPanel } from "./settings/panels/GuestsPanel";
import { PlaybackPanel } from "./settings/panels/PlaybackPanel";
import { StoragePanel } from "./settings/panels/StoragePanel";
import { SettingsShell } from "./settings/SettingsShell";
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
      <AudioCacheProvider>
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
      </AudioCacheProvider>
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
  if (path === "/liked") return <LikedSongs />;
  if (path === "/downloads") return <Downloads />;
  if (path === "/station") return <Station />;
  // Settings shell — /settings and /settings/<panel>. Bare /settings renders
  // the default panel without redirecting (no history churn); an unknown
  // panel falls back to the default too.
  m = path.match(/^\/settings(?:\/([^/]+))?$/);
  if (m) {
    const id = m[1] && SETTINGS_PANELS[m[1]] ? m[1] : DEFAULT_PANEL;
    return <SettingsRoute id={id} />;
  }

  // Legacy /diagnostics/* → /settings/* (old bookmarks / external links).
  m = path.match(/^\/diagnostics(?:\/([^/]+))?$/);
  if (m) {
    const target = m[1] && SETTINGS_PANELS[m[1]] ? m[1] : "recommender";
    return <Redirect to={`/settings/${target}`} />;
  }

  return <Home />;
}

// Renders a settings panel, downgrading admin-only ("observe") panels to
// the default for non-admins. Kept as its own component so the role hook
// runs unconditionally (Routed's body has conditional early returns).
// Fails closed: while whoami is still resolving, `useIsAdmin()` is false,
// so a deep-linked observe panel shows the default panel until identity
// confirms admin — never the reverse.
function SettingsRoute({ id }: { id: string }) {
  const isAdmin = useIsAdmin();
  const effectiveId = panelVisible(id, isAdmin) ? id : DEFAULT_PANEL;
  const Panel = SETTINGS_PANELS[effectiveId]!;
  return (
    <SettingsShell active={effectiveId}>
      <Panel />
    </SettingsShell>
  );
}

// Maps a settings/diagnostics panel id (also the URL slug) to its component.
// Single source for the router; the rail's order + labels live in
// settings/nav, keyed by the same ids.
const SETTINGS_PANELS: Record<string, ComponentType> = {
  account: AccountPanel,
  guests: GuestsPanel,
  playback: PlaybackPanel,
  appearance: AppearancePanel,
  autoplay: AutoplayPanel,
  storage: StoragePanel,
  about: AboutPanel,
  recommender: Recommender,
  listening: Listening,
  latent: LatentSpace,
  ingest: Ingest,
  tracing: Tracing,
  rum: Rum,
};

// URL-replace redirect (no extra history entry, so Back doesn't bounce off
// the old path straight back to where the user came from).
function Redirect({ to }: { to: string }) {
  useEffect(() => {
    history.replaceState(null, "", to);
    window.dispatchEvent(new PopStateEvent("popstate"));
  }, [to]);
  return null;
}
