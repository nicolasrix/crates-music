const { Icon, IconBtn, Cover, Pill, Tag, fmtDuration, fmtMs } = window.UI;

// --- Albums grid ----------------------------------------------------
function Albums({ setRoute, play }) {
  const { ALBUMS } = window.DATA;
  return (
    <main>
      <Topbar setRoute={setRoute} title="albums" />
      <section className="section">
        <div style={{display:"flex", justifyContent:"space-between", alignItems:"baseline", marginBottom: 8}}>
          <h2 style={{margin:0}}>Newest</h2>
          <span style={{color:"var(--fg-muted)", font:"var(--type-meta)"}}>{ALBUMS.length} of 60</span>
        </div>
        <p className="lead">refreshed from the gateway 4 minutes ago.</p>
        <div className="tile-grid">
          {ALBUMS.map(a => (
            <div key={a.id} className="tile" onClick={() => setRoute({ page: "album", id: a.id })}>
              <div className="cover" style={{ background: a.cover }}>
                <button className="play-overlay" onClick={(e) => { e.stopPropagation(); play(a, 0); }} title="play">
                  <Icon name="play" size={16}/>
                </button>
              </div>
              <div className="title">{a.name}</div>
              <div className="sub">{a.artist} · {a.year}</div>
            </div>
          ))}
        </div>
      </section>
    </main>
  );
}

// --- Album detail ---------------------------------------------------
function Album({ id, setRoute, play, player }) {
  const { ALBUMS, TRACKS_BY_ALBUM, defaultTracks } = window.DATA;
  const a = ALBUMS.find(x => x.id === id) || ALBUMS[0];
  const tracks = TRACKS_BY_ALBUM[a.id] || defaultTracks(a);
  const total = tracks.reduce((s, t) => s + t.duration, 0);

  return (
    <main style={{ "--art-bg": a.art.bg, "--art-fg": a.art.fg, "--art-mute": a.art.mute, "--art-accent": a.art.accent }}>
      <div className="tinted-wash"/>
      <Topbar setRoute={setRoute} title={a.name} />
      <header className="hero">
        <div className="cover-lg" style={{ background: a.cover }}/>
        <div className="meta-stack">
          <span className="kind">album</span>
          <h1>{a.name}</h1>
          <div className="sub">
            <span style={{color:"var(--art-fg)"}}>{a.artist}</span>
            <span>·</span><span>{a.year}</span>
            <span>·</span><span>{tracks.length} tracks</span>
            <span>·</span><span>{fmtDuration(total)}</span>
            <Tag tone="accent">FLAC</Tag>
            <Tag>48 kHz</Tag>
          </div>
          <div className="actions">
            <button className="play-disc" onClick={() => play(a, 0)} title="play">
              <Icon name="play" size={20}/>
            </button>
            <IconBtn title="add to queue" style={{background:"var(--surface-2)"}}><Icon name="plus" size={18}/></IconBtn>
            <IconBtn title="more" style={{background:"var(--surface-2)"}}><Icon name="more" size={18}/></IconBtn>
          </div>
        </div>
      </header>

      <section className="section" style={{paddingTop: 0}}>
        <table className="tracks">
          <thead>
            <tr><th>#</th><th>title</th><th>artist</th><th style={{textAlign:"right"}}>time</th></tr>
          </thead>
          <tbody>
            {tracks.map((t, i) => {
              const nowPlaying = player.track && player.track.albumId === a.id && player.track.title === t.title;
              return (
                <tr key={t.n} className={nowPlaying ? "now-playing" : ""} onDoubleClick={() => play(a, i)}>
                  <td className="num">
                    {nowPlaying
                      ? <span className="eq" style={{display:"inline-flex"}}><i/><i/><i/></span>
                      : t.n}
                  </td>
                  <td className="title">{t.title}</td>
                  <td className="artist">{t.artist}</td>
                  <td className="duration">{fmtDuration(t.duration)}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
        <div style={{ marginTop: 24, color: "var(--fg-faint)", font: "var(--type-meta)" }}>
          double-click a track to play. shift-click to add to queue.
        </div>
      </section>
    </main>
  );
}

// --- All tracks list ------------------------------------------------
function Tracks({ setRoute, play }) {
  const { ALBUMS, TRACKS_BY_ALBUM, defaultTracks } = window.DATA;
  const flat = ALBUMS.flatMap(a => (TRACKS_BY_ALBUM[a.id] || defaultTracks(a)).map(t => ({ ...t, album: a })));
  return (
    <main>
      <Topbar setRoute={setRoute} title="tracks" />
      <section className="section">
        <h2>All tracks</h2>
        <p className="lead">{flat.length} tracks across {ALBUMS.length} albums</p>
        <table className="tracks">
          <thead>
            <tr><th>#</th><th>title</th><th>artist</th><th>album</th><th style={{textAlign:"right"}}>time</th></tr>
          </thead>
          <tbody>
            {flat.slice(0, 30).map((t, i) => (
              <tr key={i} onDoubleClick={() => play(t.album, t.n - 1)}>
                <td className="num">{i + 1}</td>
                <td className="title">{t.title}</td>
                <td className="artist">{t.artist}</td>
                <td className="artist" onClick={(e) => {e.stopPropagation(); setRoute({page:"album", id:t.album.id})}} style={{cursor:"pointer"}}>{t.album.name}</td>
                <td className="duration">{fmtDuration(t.duration)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>
    </main>
  );
}

// --- Artists list ---------------------------------------------------
function Artists({ setRoute }) {
  const { ARTISTS } = window.DATA;
  return (
    <main>
      <Topbar setRoute={setRoute} title="artists" />
      <section className="section">
        <h2>Artists</h2>
        <p className="lead">{ARTISTS.length} in your library</p>
        <div className="tile-grid" style={{gridTemplateColumns:"repeat(auto-fill, minmax(140px, 1fr))"}}>
          {ARTISTS.map(ar => (
            <div key={ar.id} className="tile" onClick={() => setRoute({ page: "artist", id: ar.id })}>
              <div className="cover" style={{ background: `radial-gradient(circle at 30% 30%, ${ar.art.accent}, ${ar.art.bg})`, borderRadius: "50%" }}/>
              <div className="title" style={{textAlign:"center", marginTop:6}}>{ar.name}</div>
              <div className="sub" style={{textAlign:"center"}}>{ar.count} albums</div>
            </div>
          ))}
        </div>
      </section>
    </main>
  );
}

// --- Artist detail --------------------------------------------------
function Artist({ id, setRoute, play }) {
  const { ARTISTS, ALBUMS } = window.DATA;
  const ar = ARTISTS.find(x => x.id === id) || ARTISTS[0];
  const artistAlbums = ALBUMS.filter(a => a.artist === ar.name);
  return (
    <main style={{ "--art-bg": ar.art.bg, "--art-fg": ar.art.fg, "--art-mute": ar.art.mute, "--art-accent": ar.art.accent }}>
      <div className="tinted-wash" style={{height: 480}}/>
      <Topbar setRoute={setRoute} title={ar.name} />
      <header className="hero" style={{paddingBottom: 48}}>
        <div className="cover-lg" style={{ background: `radial-gradient(circle at 30% 30%, ${ar.art.accent}, ${ar.art.bg})`, borderRadius: "50%" }}/>
        <div className="meta-stack">
          <span className="kind">artist</span>
          <h1>{ar.name}</h1>
          <div className="sub">
            <span>{ar.count} tracks</span>
            <span>·</span><span>{artistAlbums.length} albums in your library</span>
          </div>
          {ar.blurb && <p style={{color: "var(--art-mute)", maxWidth: 520, marginTop: 8}}>{ar.blurb}</p>}
          <div className="actions">
            <button className="play-disc" onClick={() => play(artistAlbums[0] || ALBUMS[0], 0)} title="play"><Icon name="play" size={20}/></button>
          </div>
        </div>
      </header>
      <section className="section">
        <h2 style={{font:"var(--type-h3)"}}>Albums</h2>
        <div className="tile-grid">
          {artistAlbums.map(a => (
            <div key={a.id} className="tile" onClick={() => setRoute({ page: "album", id: a.id })}>
              <div className="cover" style={{ background: a.cover }}/>
              <div className="title">{a.name}</div>
              <div className="sub">{a.year}</div>
            </div>
          ))}
        </div>
      </section>
    </main>
  );
}

// --- Playlist -------------------------------------------------------
function Playlist({ id, setRoute, play, player }) {
  const { PLAYLISTS, ALBUMS, TRACKS_BY_ALBUM, defaultTracks } = window.DATA;
  const p = PLAYLISTS.find(x => x.id === id) || PLAYLISTS[0];
  // Synthesize a track list from the first 3 albums.
  const seed = ALBUMS.slice(0, 4);
  const tracks = seed.flatMap(a =>
    (TRACKS_BY_ALBUM[a.id] || defaultTracks(a)).slice(0, 3).map(t => ({...t, album: a}))
  );
  const total = tracks.reduce((s, t) => s + t.duration, 0);

  return (
    <main style={{ "--art-bg": p.art.bg, "--art-fg": p.art.fg, "--art-mute": p.art.mute, "--art-accent": p.art.accent }}>
      <div className="tinted-wash"/>
      <Topbar setRoute={setRoute} title={p.name} />
      <header className="hero">
        <div className="cover-lg" style={{
          background: `linear-gradient(135deg, ${p.art.bg}, ${p.art.accent})`,
          display:"grid", gridTemplate:"1fr 1fr / 1fr 1fr", overflow:"hidden"
        }}>
          {seed.slice(0,4).map(a => <div key={a.id} style={{background: a.cover}}/>)}
        </div>
        <div className="meta-stack">
          <span className="kind">playlist</span>
          <h1>{p.name}</h1>
          <div className="sub">
            <span>{tracks.length} tracks</span>
            <span>·</span><span>{fmtDuration(total)}</span>
          </div>
          <div className="actions">
            <button className="play-disc" onClick={() => play(seed[0], 0)} title="play"><Icon name="play" size={20}/></button>
            <IconBtn style={{background:"var(--surface-2)"}}><Icon name="more" size={18}/></IconBtn>
          </div>
        </div>
      </header>
      <section className="section" style={{paddingTop: 0}}>
        <table className="tracks">
          <thead><tr><th>#</th><th>title</th><th>album</th><th style={{textAlign:"right"}}>time</th></tr></thead>
          <tbody>
            {tracks.map((t, i) => (
              <tr key={i} onDoubleClick={() => play(t.album, t.n - 1)}>
                <td className="num">{i + 1}</td>
                <td className="title">{t.title}</td>
                <td className="artist">{t.album.name}</td>
                <td className="duration">{fmtDuration(t.duration)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>
    </main>
  );
}

// --- Diagnostics ----------------------------------------------------
function Diagnostics({ setRoute }) {
  const { DIAG_QUEUE, DIAG_HIST, DIAG_RUM } = window.DATA;
  const maxMs = Math.max(...DIAG_HIST.map(b => b.max_ms));
  return (
    <main>
      <Topbar setRoute={setRoute} title="diagnostics" />
      <section className="section">
        <h2>Diagnostics</h2>
        <p className="lead">5 s refresh · authenticated against the gateway</p>

        <div className="diag-section">
          <h3>Ingest queue</h3>
          <p style={{font:"var(--type-mono)", fontSize:11, color:"var(--fg-faint)"}}>model: {DIAG_QUEUE.model_version}</p>
          <div style={{display:"grid", gridTemplateColumns:"repeat(auto-fill, minmax(160px, 1fr))", gap: 12}}>
            <div className="tile-stat"><div className="label">not started</div><div className="value">{DIAG_QUEUE.not_started}</div></div>
            <div className="tile-stat"><div className="label">in progress</div><div className="value" style={{color:"var(--warning-200)"}}>{DIAG_QUEUE.in_progress}</div></div>
            <div className="tile-stat"><div className="label">done</div><div className="value" style={{color:"var(--success-200)"}}>{DIAG_QUEUE.done.toLocaleString()}</div></div>
            <div className="tile-stat"><div className="label">failed</div><div className="value" style={{color: DIAG_QUEUE.failed > 0 ? "var(--danger-200)" : "var(--fg-faint)"}}>{DIAG_QUEUE.failed}</div></div>
          </div>
        </div>

        <div className="diag-section">
          <h3>Span duration histogram</h3>
          <table className="diag-table">
            <thead>
              <tr><th>name</th><th className="num">count</th><th className="num">p50</th><th className="num">p95</th><th className="num">p99</th><th className="num">max</th><th>distribution</th></tr>
            </thead>
            <tbody>
              {DIAG_HIST.map(b => (
                <tr key={b.name}>
                  <td>{b.name}</td>
                  <td className="num">{b.count.toLocaleString()}</td>
                  <td className="num">{fmtMs(b.p50_ms)}</td>
                  <td className="num">{fmtMs(b.p95_ms)}</td>
                  <td className="num">{fmtMs(b.p99_ms)}</td>
                  <td className="num" style={{color:"var(--fg-faint)"}}>{fmtMs(b.max_ms)}</td>
                  <td style={{minWidth: 160}}>
                    <div className="boxbar">
                      <div className="seg s3" style={{width: `${(b.p99_ms/maxMs)*100}%`}}/>
                      <div className="seg s2" style={{width: `${(b.p95_ms/maxMs)*100}%`}}/>
                      <div className="seg s1" style={{width: `${(b.p50_ms/maxMs)*100}%`}}/>
                      <div className="max" style={{left: `${(b.max_ms/maxMs)*100}%`}}/>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>

        <div className="diag-section">
          <h3>Client events (RUM)</h3>
          <table className="diag-table">
            <thead><tr><th>received</th><th>name</th><th className="num">value</th><th>rating</th><th>page</th><th>session</th></tr></thead>
            <tbody>
              {DIAG_RUM.map((e, i) => (
                <tr key={i}>
                  <td style={{color:"var(--fg-muted)"}}>{e.time}</td>
                  <td>{e.name}</td>
                  <td className="num">{fmtMs(e.value_ms)}</td>
                  <td>{e.rating ? <Pill tone={e.rating}>{e.rating}</Pill> : "—"}</td>
                  <td style={{color:"var(--fg-muted)"}}>{e.page}</td>
                  <td style={{color:"var(--fg-faint)"}}>{e.session}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </section>
    </main>
  );
}

// --- Top bar (back + breadcrumb) ------------------------------------
function Topbar({ setRoute, title }) {
  return (
    <div className="topbar">
      <div className="nav-arrows">
        <button className="arrow" onClick={() => history.back()} title="back"><Icon name="chevron-l" size={16}/></button>
        <button className="arrow" onClick={() => history.forward()} title="forward"><Icon name="chevron-r" size={16}/></button>
      </div>
      <div className="breadcrumb">{title}</div>
      <div style={{marginLeft:"auto"}}>
        <IconBtn title="settings" style={{color:"var(--fg-muted)"}}><Icon name="settings" size={16}/></IconBtn>
      </div>
    </div>
  );
}

window.Pages = { Albums, Album, Tracks, Artists, Artist, Playlist, Diagnostics };
