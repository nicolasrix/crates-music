/* Sample data — jazz/funk classics with synthetic cover gradients.
   Each album defines its own --art-* palette so the page tints itself
   from the artwork (the system's central color rule). */

const ALBUMS = [
  { id: "a1", name: "Bitches Brew", artist: "Miles Davis", year: 1970, count: 14,
    cover: "linear-gradient(135deg, #3A1F0E 0%, #9A6B3F 60%, #E8A96A 100%)",
    art: { bg:"#2A1607", fg:"#F4D9B7", mute:"#9A8267", accent:"#E8A96A" } },
  { id: "a2", name: "Blue Train", artist: "John Coltrane", year: 1957, count: 7,
    cover: "linear-gradient(135deg, #0F2236 0%, #3E6B92 60%, #C8DAEC 100%)",
    art: { bg:"#0B1A2A", fg:"#D7E5F0", mute:"#7990A6", accent:"#5B8FB9" } },
  { id: "a3", name: "Pet Sounds", artist: "The Beach Boys", year: 1966, count: 13,
    cover: "linear-gradient(135deg, #1B2A1A 0%, #5E7A4A 60%, #A8C26B 100%)",
    art: { bg:"#16241B", fg:"#DBE7CC", mute:"#869682", accent:"#A8C26B" } },
  { id: "a4", name: "Maggot Brain", artist: "Funkadelic", year: 1971, count: 6,
    cover: "linear-gradient(135deg, #2A1A2A 0%, #6E3F75 60%, #B07AA1 100%)",
    art: { bg:"#1F0F23", fg:"#E8D6EA", mute:"#9A839F", accent:"#B07AA1" } },
  { id: "a5", name: "In a Silent Way", artist: "Miles Davis", year: 1969, count: 4,
    cover: "linear-gradient(135deg, #14181C 0%, #3A4148 60%, #B8BFC6 100%)",
    art: { bg:"#0F1216", fg:"#D8DDE3", mute:"#7E858D", accent:"#A0AAB4" } },
  { id: "a6", name: "Innervisions", artist: "Stevie Wonder", year: 1973, count: 9,
    cover: "linear-gradient(135deg, #1F1F0E 0%, #5E5928 60%, #C7B95E 100%)",
    art: { bg:"#1B1A0E", fg:"#EFE6BA", mute:"#998E5F", accent:"#C7B95E" } },
  { id: "a7", name: "Songs in the Key of Life", artist: "Stevie Wonder", year: 1976, count: 21,
    cover: "linear-gradient(135deg, #2D1216 0%, #8A2630 60%, #D86C77 100%)",
    art: { bg:"#1F0B10", fg:"#F0CFD3", mute:"#A8767C", accent:"#D86C77" } },
  { id: "a8", name: "Headhunters", artist: "Herbie Hancock", year: 1973, count: 4,
    cover: "linear-gradient(135deg, #14252B 0%, #2D6273 60%, #6FB3C2 100%)",
    art: { bg:"#0E1B20", fg:"#CDE3E9", mute:"#7C9DA6", accent:"#6FB3C2" } },
  { id: "a9", name: "A Love Supreme", artist: "John Coltrane", year: 1965, count: 4,
    cover: "linear-gradient(135deg, #0B0E10 0%, #2A2E33 60%, #7A8086 100%)",
    art: { bg:"#0B0E10", fg:"#D6DADD", mute:"#7A8086", accent:"#A6ACB1" } },
  { id: "a10", name: "There's a Riot Goin' On", artist: "Sly & The Family Stone", year: 1971, count: 11,
    cover: "linear-gradient(135deg, #2A0F0F 0%, #75221F 60%, #C16A52 100%)",
    art: { bg:"#1F0A0A", fg:"#F0CDC2", mute:"#A8786B", accent:"#C16A52" } },
];

const TRACKS_BY_ALBUM = {
  a1: [
    { n: 1, title: "Pharaoh's Dance", artist: "Miles Davis", duration: 1205 },
    { n: 2, title: "Bitches Brew", artist: "Miles Davis", duration: 1618 },
    { n: 3, title: "Spanish Key", artist: "Miles Davis", duration: 1052 },
    { n: 4, title: "John McLaughlin", artist: "Miles Davis", duration: 274 },
    { n: 5, title: "Miles Runs the Voodoo Down", artist: "Miles Davis", duration: 851 },
    { n: 6, title: "Sanctuary", artist: "Miles Davis", duration: 651 },
  ],
  a2: [
    { n: 1, title: "Blue Train", artist: "John Coltrane", duration: 624 },
    { n: 2, title: "Moment's Notice", artist: "John Coltrane", duration: 543 },
    { n: 3, title: "Locomotion", artist: "John Coltrane", duration: 432 },
    { n: 4, title: "I'm Old Fashioned", artist: "John Coltrane", duration: 437 },
    { n: 5, title: "Lazy Bird", artist: "John Coltrane", duration: 425 },
  ],
  a4: [
    { n: 1, title: "Maggot Brain", artist: "Funkadelic", duration: 612 },
    { n: 2, title: "Can You Get to That", artist: "Funkadelic", duration: 173 },
    { n: 3, title: "Hit It and Quit It", artist: "Funkadelic", duration: 219 },
    { n: 4, title: "You and Your Folks, Me and My Folks", artist: "Funkadelic", duration: 238 },
    { n: 5, title: "Super Stupid", artist: "Funkadelic", duration: 235 },
    { n: 6, title: "Back in Our Minds", artist: "Funkadelic", duration: 156 },
    { n: 7, title: "Wars of Armageddon", artist: "Funkadelic", duration: 562 },
  ],
};

// Default expanded tracks for any other album
function defaultTracks(album) {
  const base = ["Opener", "Stride", "Interlude", "Long Form", "Coda"];
  return base.map((title, i) => ({
    n: i + 1, title: `${title}`, artist: album.artist,
    duration: 180 + i * 90 + (i % 2 ? 30 : 0)
  }));
}

const PLAYLISTS = [
  { id: "p1", name: "late nights", count: 28, art: ALBUMS[0].art },
  { id: "p2", name: "kitchen + coffee", count: 41, art: ALBUMS[5].art },
  { id: "p3", name: "long drives", count: 67, art: ALBUMS[3].art },
];

const ARTISTS = [
  { id: "ar1", name: "Miles Davis", count: 14, art: ALBUMS[0].art, blurb: "Trumpet. Reinvented jazz approximately every five years between 1949 and 1980." },
  { id: "ar2", name: "John Coltrane", count: 22, art: ALBUMS[1].art },
  { id: "ar3", name: "Funkadelic", count: 9, art: ALBUMS[3].art },
  { id: "ar4", name: "Stevie Wonder", count: 16, art: ALBUMS[5].art },
  { id: "ar5", name: "Herbie Hancock", count: 11, art: ALBUMS[7].art },
];

// Diagnostics sample mirrors the schema in apps/web/src/api/diagnostics.ts
const DIAG_QUEUE = { model_version: "clap-music-2.5", not_started: 142, in_progress: 4, done: 8231, failed: 2 };
const DIAG_HIST = [
  { name: "embedder.embed_audio", count: 612, min_ms: 84, p50_ms: 442, p95_ms: 1310, p99_ms: 3040, max_ms: 8204 },
  { name: "ann.query",            count: 22841, min_ms: 0.04, p50_ms: 0.098, p95_ms: 0.112, p99_ms: 0.140, max_ms: 2.1 },
  { name: "subsonic.getAlbum",    count: 184, min_ms: 12, p50_ms: 28, p95_ms: 96, p99_ms: 240, max_ms: 612 },
  { name: "trace_store.insert",   count: 9034, min_ms: 0.18, p50_ms: 0.25, p95_ms: 0.43, p99_ms: 0.81, max_ms: 12.4 },
];
const DIAG_RUM = [
  { time: "14:02:04", name: "playback.start",   value_ms: 184, rating: "good", page: "/album/a1", session: "9c3a1f4e" },
  { time: "14:02:01", name: "web-vital.LCP",    value_ms: 1820, rating: "good", page: "/", session: "9c3a1f4e" },
  { time: "14:01:58", name: "web-vital.INP",    value_ms: 96,   rating: "good", page: "/", session: "9c3a1f4e" },
  { time: "14:01:42", name: "playback.start",   value_ms: 1420, rating: "needs-improvement", page: "/album/a4", session: "9c3a1f4e" },
  { time: "14:01:11", name: "web-vital.CLS",    value_ms: null, rating: "good", page: "/", session: "9c3a1f4e" },
];

window.DATA = { ALBUMS, TRACKS_BY_ALBUM, PLAYLISTS, ARTISTS, DIAG_QUEUE, DIAG_HIST, DIAG_RUM, defaultTracks };
