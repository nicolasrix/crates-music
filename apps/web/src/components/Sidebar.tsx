// Left navigation. 240px fixed width; brand row + search + nav-groups for
// browse / playlists / system. The active state has a 2px amber rule offset
// from the top/bottom of the row — see .nav-item.is-active::before in
// styles/components.css.
//
// Each browse section (albums / artists / tracks) has three sub-items —
// recently added / most played / random — rendered as indented sub-rows
// below the parent. Sub-items match the URL exactly; the parent stays
// active for any sub-page (via `prefix`).

import { Boxes, Disc3, Download, Heart, Home as HomeIcon, ListMusic, Radio, User, Search, Plus, SlidersHorizontal } from "lucide-react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { createPlaylist, listPlaylists } from "../api/playlists";
import { Link, navigate, useRoute } from "../router";
import { useToast } from "../toast/ToastContext";
import { BrandMark } from "./BrandMark";

interface SubItem {
  to: string;
  label: string;
}

interface NavItem {
  to: string;
  label: string;
  icon: React.ReactNode;
  /** When set, the item is active for any path that starts with this
   *  prefix (so /albums/abc still highlights "albums"). */
  prefix?: string;
  /** Sub-items rendered below the parent. They share the parent's prefix
   *  for active highlighting; matching uses exact path equality. */
  subs?: readonly SubItem[];
}

const SECTION_SUBS = [
  { to: "/recent", label: "recently added" },
  { to: "/most-played", label: "most played" },
  { to: "/random", label: "random" },
] as const;

const BROWSE: NavItem[] = [
  { to: "/", label: "home", icon: <HomeIcon size={18} strokeWidth={1.5} /> },
  {
    to: "/albums",
    label: "albums",
    icon: <Disc3 size={18} strokeWidth={1.5} />,
    prefix: "/albums",
    subs: SECTION_SUBS.map((s) => ({ to: `/albums${s.to}`, label: s.label })),
  },
  {
    to: "/artists",
    label: "artists",
    icon: <User size={18} strokeWidth={1.5} />,
    prefix: "/artists",
    subs: SECTION_SUBS.map((s) => ({ to: `/artists${s.to}`, label: s.label })),
  },
  {
    to: "/tracks",
    label: "tracks",
    icon: <ListMusic size={18} strokeWidth={1.5} />,
    prefix: "/tracks",
    subs: SECTION_SUBS.map((s) => ({ to: `/tracks${s.to}`, label: s.label })),
  },
  {
    to: "/crates",
    label: "crates",
    icon: <Boxes size={18} strokeWidth={1.5} />,
  },
  {
    to: "/liked",
    label: "liked songs",
    icon: <Heart size={18} strokeWidth={1.5} />,
  },
  {
    to: "/downloads",
    label: "downloads",
    icon: <Download size={18} strokeWidth={1.5} />,
  },
  {
    to: "/station",
    label: "station",
    icon: <Radio size={18} strokeWidth={1.5} />,
  },
];

const SYSTEM: NavItem[] = [
  {
    // Settings now also hosts diagnostics (interleaved by domain in the
    // settings rail), so the sidebar's "system" group is a single link.
    to: "/settings/account",
    label: "settings",
    icon: <SlidersHorizontal size={18} strokeWidth={1.5} />,
    prefix: "/settings",
  },
];

interface SidebarProps {
  /** Drawer state — only meaningful ≤768px where the sidebar is
   *  off-canvas; on desktop the class has no effect. */
  open?: boolean;
  /** Called when a navigation inside the sidebar should dismiss the
   *  drawer (same-page navigations don't remount Layout, so closing
   *  can't rely on remount alone). */
  onClose?: () => void;
}

export function Sidebar({ open = false, onClose }: SidebarProps = {}) {
  const { path, search } = useRoute();
  const isParentActive = (item: NavItem) => {
    if (item.prefix && (path === item.prefix || path.startsWith(item.prefix + "/"))) return true;
    return path === item.to;
  };
  // Sub-rows match the URL exactly. The bare section path (e.g. /albums)
  // is the "all" view and lights up the parent only — no sub gets the
  // active state on bare paths.
  const isSubActive = (_parent: NavItem, sub: SubItem) => path === sub.to;

  // Keep the input controlled. When the user is on /search?q=foo, mirror
  // that into the input so back/forward in browser history reflects in the
  // box. We don't navigate per keystroke — only on Enter — so live typing
  // doesn't push history entries.
  const initial =
    path === "/search"
      ? (new URLSearchParams(search).get("q") ?? "")
      : "";
  const [query, setQuery] = useState(initial);
  useEffect(() => {
    if (path === "/search") {
      setQuery(new URLSearchParams(search).get("q") ?? "");
    }
  }, [path, search]);

  function submitSearch(e: React.FormEvent) {
    e.preventDefault();
    const q = query.trim();
    if (q.length === 0) return;
    navigate(`/search?q=${encodeURIComponent(q)}`);
    onClose?.(); // it's a navigation — dismiss the drawer like a link click
  }

  const queryClient = useQueryClient();
  const toast = useToast();
  const [creating, setCreating] = useState(false);
  // Shared cache key with TrackRowMenu's playlist picker — both surfaces
  // refetch via the same `["playlists"]` invalidation after a create or
  // edit, so navigating away and back doesn't cause a redundant fetch.
  const playlistsQ = useQuery({
    queryKey: ["playlists"],
    queryFn: listPlaylists,
    staleTime: 60_000,
  });
  // Lightweight playlist creation — prompt() is sufficient for a single-
  // user app and avoids introducing a modal primitive that nothing else
  // uses. Once any other surface needs a modal, swap this for a proper
  // controlled dialog.
  async function newPlaylist() {
    if (creating) return;
    const raw = window.prompt("playlist name");
    if (raw === null) return;
    const name = raw.trim();
    if (name.length === 0) return;
    setCreating(true);
    try {
      const created = await createPlaylist(name);
      // Refresh the playlists query so any future sidebar/playlists list
      // picks the new one up.
      await queryClient.invalidateQueries({ queryKey: ["playlists"] });
      if (created.id) {
        navigate(`/playlists/${created.id}`);
      }
    } catch (e) {
      toast(`couldn't create playlist: ${(e as Error).message}`, {
        variant: "error",
      });
    } finally {
      setCreating(false);
    }
  }

  return (
    <aside
      className={`sidebar ${open ? "is-open" : ""}`}
      // Delegated close-on-navigate: any anchor click inside the drawer
      // (nav items, sub-items, playlists) dismisses it. Buttons (search
      // submit, new-playlist) intentionally keep it open.
      onClick={(e) => {
        if (onClose && (e.target as HTMLElement).closest("a")) onClose();
      }}
    >
      <div className="brand">
        <BrandMark size={28} />
        <span className="word">crates</span>
      </div>

      <div className="search-box">
        <form onSubmit={submitSearch} className="relative">
          <span className="absolute left-2 top-1/2 -translate-y-1/2 text-fg-faint pointer-events-none">
            <Search size={14} strokeWidth={1.5} />
          </span>
          <input
            type="search"
            placeholder="search albums, artists, tracks…"
            className="search-input"
            aria-label="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </form>
      </div>

      <div className="nav-group">browse</div>
      {BROWSE.map((item) => (
        <div key={item.to}>
          <Link
            to={item.to}
            className={`nav-item ${isParentActive(item) ? "is-active" : ""}`}
          >
            {item.icon}
            <span>{item.label}</span>
          </Link>
          {item.subs && (
            <div className="nav-subs">
              {item.subs.map((sub) => (
                <Link
                  key={sub.to}
                  to={sub.to}
                  className={`nav-item is-sub ${isSubActive(item, sub) ? "is-active" : ""}`}
                >
                  <span>{sub.label}</span>
                </Link>
              ))}
            </div>
          )}
        </div>
      ))}

      <div className="nav-group">playlists</div>
      <button
        className="nav-item nav-item-button"
        onClick={newPlaylist}
        disabled={creating}
        type="button"
      >
        <Plus size={18} strokeWidth={1.5} />
        <span>{creating ? "creating…" : "new playlist"}</span>
      </button>
      {playlistsQ.data && playlistsQ.data.length > 0 && (
        <div className="nav-subs">
          {playlistsQ.data.map((p) => {
            const to = `/playlists/${p.id}`;
            return (
              <Link
                key={p.id}
                to={to}
                className={`nav-item is-sub ${path === to ? "is-active" : ""}`}
              >
                <span className="truncate" title={p.name}>{p.name}</span>
              </Link>
            );
          })}
        </div>
      )}

      <div className="nav-group" style={{ marginTop: "auto", paddingTop: "var(--space-5)" }}>
        system
      </div>
      {SYSTEM.map((item) => (
        <div key={item.to}>
          <Link
            to={item.to}
            className={`nav-item ${isParentActive(item) ? "is-active" : ""}`}
          >
            {item.icon}
            <span>{item.label}</span>
          </Link>
          {item.subs && (
            <div className="nav-subs">
              {item.subs.map((sub) => (
                <Link
                  key={sub.to}
                  to={sub.to}
                  className={`nav-item is-sub ${isSubActive(item, sub) ? "is-active" : ""}`}
                >
                  <span>{sub.label}</span>
                </Link>
              ))}
            </div>
          )}
        </div>
      ))}
    </aside>
  );
}
