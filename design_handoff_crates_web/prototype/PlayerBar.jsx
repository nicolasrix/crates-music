const { Icon, IconBtn, fmtDuration } = window.UI;

function PlayerBar({ player, setPlayer }) {
  const t = player.track;
  const dur = t?.duration ?? 0;
  const pct = dur ? Math.min(100, (player.position / dur) * 100) : 0;

  // Animate the scrubber while playing.
  React.useEffect(() => {
    if (!player.isPlaying || !t) return;
    const id = setInterval(() => {
      setPlayer(p => {
        if (!p.isPlaying || !p.track) return p;
        const next = p.position + 1;
        if (next >= p.track.duration) return { ...p, position: 0, isPlaying: p.autoplay };
        return { ...p, position: next };
      });
    }, 1000);
    return () => clearInterval(id);
  }, [player.isPlaying, t]);

  const seek = (e) => {
    const rect = e.currentTarget.getBoundingClientRect();
    const ratio = (e.clientX - rect.left) / rect.width;
    setPlayer(p => ({ ...p, position: Math.round(ratio * (p.track?.duration || 1)) }));
  };

  if (!t) {
    return (
      <div className="player">
        <div style={{color:"var(--fg-faint)", padding: "0 12px"}}>nothing playing — pick an album.</div>
        <div/><div/>
      </div>
    );
  }

  return (
    <div className="player" style={{ "--art-accent": t.art?.accent || "var(--accent)" }}>
      <div className="np">
        <div className="cover" style={{ background: t.albumCover || "var(--surface-2)" }}/>
        <div className="meta">
          <div className="title">{t.title}</div>
          <div className="sub">{t.artist} · {t.albumName}</div>
        </div>
      </div>

      <div className="transport-col">
        <div className="transport">
          <IconBtn title="repeat"   on={player.repeat}   onClick={() => setPlayer(p => ({...p, repeat: !p.repeat}))}><Icon name="repeat"/></IconBtn>
          <IconBtn title="previous" onClick={player.onPrev}><Icon name="skip-back"/></IconBtn>
          <IconBtn className="play-bar" title={player.isPlaying ? "pause" : "play"}
                   onClick={() => setPlayer(p => ({...p, isPlaying: !p.isPlaying}))}>
            <Icon name={player.isPlaying ? "pause" : "play"} size={16}/>
          </IconBtn>
          <IconBtn title="next" onClick={player.onNext}><Icon name="skip-fwd"/></IconBtn>
          <IconBtn title="autoplay (recommends + plays next)" on={player.autoplay}
                   onClick={() => setPlayer(p => ({...p, autoplay: !p.autoplay}))}><Icon name="auto"/></IconBtn>
        </div>
        <div className="scrub">
          <span className="time">{fmtDuration(player.position)}</span>
          <div className="scrub-bar" onClick={seek}>
            <div className="fill" style={{ width: `${pct}%` }}/>
            <div className="thumb" style={{ left: `${pct}%` }}/>
          </div>
          <span className="time">{fmtDuration(dur)}</span>
        </div>
      </div>

      <div className="right-cluster">
        <button className={`autoplay-toggle ${player.autoplay ? "on" : ""}`}
                onClick={() => setPlayer(p => ({...p, autoplay: !p.autoplay}))}>
          <span className="dot"/>autoplay {player.autoplay ? "on" : "off"}
        </button>
      </div>
    </div>
  );
}

window.PlayerBar = PlayerBar;
