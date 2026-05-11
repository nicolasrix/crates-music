const { Icon, Search } = window.UI;

function Sidebar({ route, setRoute, onSearch, query }) {
  const { PLAYLISTS } = window.DATA;
  const NavItem = ({ id, icon, label, muted }) => (
    <div
      className={`nav-item ${route.page === id ? "active" : ""} ${muted ? "muted" : ""}`}
      onClick={() => setRoute({ page: id })}
    >
      {icon ? <Icon name={icon} size={16}/> : null}
      <span>{label}</span>
    </div>
  );

  return (
    <aside className="sidebar">
      <div className="brand">
        <svg className="mark" viewBox="0 0 64 64">
          <circle cx="32" cy="32" r="30" fill="var(--ink-100)"/>
          <circle cx="32" cy="32" r="22" fill="none" stroke="rgba(255,255,255,0.05)" strokeWidth="0.6"/>
          <circle cx="32" cy="32" r="16" fill="none" stroke="rgba(255,255,255,0.05)" strokeWidth="0.6"/>
          <circle cx="32" cy="32" r="11" fill="var(--accent)"/>
          <circle cx="32" cy="32" r="1.4" fill="var(--ink-50)"/>
        </svg>
        <span className="word">crates</span>
      </div>
      <div className="search-box">
        <Search value={query} onChange={onSearch} />
      </div>
      <div className="nav-group">browse</div>
      <NavItem id="albums"  icon="disc"    label="albums" />
      <NavItem id="artists" icon="user"    label="artists" />
      <NavItem id="tracks"  icon="list"    label="tracks" />
      <div className="nav-group">playlists</div>
      <div className="nav-item muted" onClick={() => alert("creates a new playlist (mock)")}>
        <Icon name="plus" size={16}/><span>new playlist</span>
      </div>
      {PLAYLISTS.map(p => (
        <div key={p.id}
             className={`nav-item ${route.page === "playlist" && route.id === p.id ? "active" : ""}`}
             onClick={() => setRoute({ page: "playlist", id: p.id })}>
          <span style={{ display:"inline-block", width: 12, height: 12, borderRadius: 3, background: p.art.accent, opacity: 0.85 }}/>
          <span>{p.name}</span>
          <span style={{ marginLeft: "auto", color: "var(--fg-faint)", fontSize: 11, fontFamily:"var(--font-mono)" }}>{p.count}</span>
        </div>
      ))}
      <div className="nav-group" style={{ marginTop: "auto", paddingTop: 24 }}>system</div>
      <NavItem id="diagnostics" icon="diagnostics" label="diagnostics" />
    </aside>
  );
}

window.Sidebar = Sidebar;
