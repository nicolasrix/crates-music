const { useState } = React;
const { Albums, Album, Tracks, Artists, Artist, Playlist, Diagnostics } = window.Pages;

function App() {
  const [route, setRoute] = useState({ page: "albums" });
  const [query, setQuery] = useState("");
  const [player, setPlayer] = useState({
    track: null, queue: [], queueIndex: 0,
    isPlaying: false, position: 0,
    repeat: false, autoplay: true,
    onPrev: () => {}, onNext: () => {},
  });

  function makeQueue(album, startIndex) {
    const { TRACKS_BY_ALBUM, defaultTracks } = window.DATA;
    const tracks = TRACKS_BY_ALBUM[album.id] || defaultTracks(album);
    const queue = tracks.map(t => ({
      ...t, albumId: album.id, albumName: album.name, albumCover: album.cover, art: album.art
    }));
    return { queue, startIndex };
  }

  const play = (album, startIndex = 0) => {
    const { queue, startIndex: si } = makeQueue(album, startIndex);
    setPlayer(p => ({
      ...p,
      queue, queueIndex: si,
      track: queue[si],
      position: 0,
      isPlaying: true,
      onNext: () => setPlayer(p => {
        const ni = p.queueIndex + 1;
        if (ni < p.queue.length) return { ...p, queueIndex: ni, track: p.queue[ni], position: 0, isPlaying: true };
        return { ...p, isPlaying: false };
      }),
      onPrev: () => setPlayer(p => {
        const ni = Math.max(0, p.queueIndex - 1);
        return { ...p, queueIndex: ni, track: p.queue[ni], position: 0, isPlaying: true };
      }),
    }));
  };

  let page;
  if (route.page === "albums")       page = <Albums setRoute={setRoute} play={play}/>;
  else if (route.page === "album")   page = <Album id={route.id} setRoute={setRoute} play={play} player={player}/>;
  else if (route.page === "tracks")  page = <Tracks setRoute={setRoute} play={play}/>;
  else if (route.page === "artists") page = <Artists setRoute={setRoute}/>;
  else if (route.page === "artist")  page = <Artist id={route.id} setRoute={setRoute} play={play}/>;
  else if (route.page === "playlist")page = <Playlist id={route.id} setRoute={setRoute} play={play} player={player}/>;
  else if (route.page === "diagnostics") page = <Diagnostics setRoute={setRoute}/>;
  else page = <Albums setRoute={setRoute} play={play}/>;

  return (
    <div className="shell">
      <window.Sidebar route={route} setRoute={setRoute} onSearch={setQuery} query={query}/>
      {page}
      <div className="player">
        <window.PlayerBar player={player} setPlayer={setPlayer}/>
      </div>
    </div>
  );
}

ReactDOM.createRoot(document.getElementById("root")).render(<App/>);
