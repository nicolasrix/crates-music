# music-subsonic

**Path:** `crates/music-subsonic/`
**Type:** library
**Test count:** 35

Typed Subsonic / OpenSubsonic HTTP client. Implements only the
endpoints crates-music actually uses; not a general-purpose Subsonic
SDK.

## Scope

Currently implemented:

| Subsonic endpoint | Method | Returns |
|---|---|---|
| `/rest/ping` | `Client::ping` | `()` |
| `/rest/getAlbumList2` | `Client::get_album_list2` | `Vec<Album>` |
| `/rest/getAlbum` | `Client::get_album` | `AlbumWithSongs` |
| `/rest/getSong` | `Client::get_song` | `Track` |
| `/rest/getArtists` | `Client::get_artists` | `Vec<Artist>` |
| `/rest/getArtist` | `Client::get_artist` | `ArtistWithAlbums` |
| `/rest/search3` | `Client::search3` | `SearchResult3` |
| `/rest/stream` | `Client::stream_url` | `Url` (caller does the GET) |

`stream_url` only *builds* the authenticated URL; the caller performs
the GET (potentially with `Range` headers) so it can wire bytes
straight into a decoder, a cache, or both. This crate never streams
bytes itself.

`get_song` exists for the ingest pipeline: it reads
`duration_seconds` so the gateway can pick a `timeOffset` for the
embedding clip window (see the perf note below). `get_artists` /
`get_artist` / `search3` back the CLI's browse parity (`music
artists`, `music artist <id>`, `music tracks`, `music search`).
`search3` applies `count`/`offset` to all three buckets at once; an
empty `query` matches everything on Navidrome, which is how the full
track list is paged.

Not implemented (but trivial to add by following the same pattern):
playlists, scrobble. The gateway proxies these via pass-through
(`/rest/*` → Navidrome), so clients can hit them through Subsonic
SDKs even though this crate doesn't model them.

## Public API

```rust
use music_subsonic::{Client, Credentials};

let creds = Credentials {
    username: "alice",
    password: "wonderland",
};
let client = Client::new("http://nav.lan:4533", creds)?;
let ping = client.ping().await?;
let albums = client
    .get_album_list2(AlbumListType::Newest, Some(100), Some(0))
    .await?;
```

`get_album_list2` takes `Option<u32>` for `size`/`offset` (omit the
param when `None`), not bare counts.

For the gateway-proxy mode (CLI talking to gateway, not Navidrome),
`with_bearer` and `with_tls` are chained builder methods on a
`Client::new(...)`; each returns `Result<Self>` and the two compose
in either order:

```rust
let client = Client::new("https://gateway.local:8443", creds)?
    .with_tls(Some(&ca_pem), false)?   // trust the mkcert CA
    .with_bearer("<token>")?;
let albums = client
    .get_album_list2(AlbumListType::Newest, Some(100), Some(0))
    .await?;
```

`with_bearer` adds an `Authorization: Bearer <token>` header to every
request. The Subsonic `t+s` params are still appended but the gateway
ignores them (it strips client-supplied Subsonic auth) — bearer is
what gates the proxy. Note `creds` are still required to construct the
client even in gateway mode, since the same code path builds the
Subsonic query string.

### TLS trust (`with_tls`)

The workspace builds reqwest with `rustls-tls`, whose trust anchors
are the **bundled webpki roots — not the system store**. A private CA
(e.g. an mkcert `gateway.local` cert) is therefore invisible to the
client; `mkcert -install` into the OS trust store does nothing here.
`with_tls(ca_cert_pem, insecure)` exists to bridge that: `ca_cert_pem`
is a PEM bundle added as an *extra* root on top of the built-ins, and
`insecure` disables verification entirely (debug/throwaway only). This
mirrors the gateway HTTP client's `[gateway].ca_cert_path` /
`insecure_tls` knobs, so gateway-mode browse and recommend
title-resolution verify the same cert the ratings/sync paths already
do. An empty/misnamed PEM file (zero certs parsed) is a loud
`Error::Config` rather than a silently-untrusted client.

## Auth

Subsonic auth is `t = md5(password + salt)`, where `salt` is a fresh
random string per request. The `Credentials` struct generates `salt`
internally and sends `u`, `t`, `s`, `v`, `c`, `f` as query params on
every request. None of this leaks into the public API — callers just
construct a `Client` once.

OpenSubsonic adds a `password` form variant; we use the legacy `t+s`
variant because Navidrome supports both and the legacy form is what
existing clients use.

## URL handling

The base URL accepts either `http://host` or `http://host/`.
`Url::join` requires a trailing slash to keep the host as the base, so
we normalize internally:

> "Allow callers to pass either 'http://host' or 'http://host/' —
> Url::join requires a trailing slash to keep the host as the base."
> *— `crates/music-subsonic/src/lib.rs`*

This is a small thing but it's saved several "why is my URL `nav.lan`
joined to give me `/rest/...` instead of `http://nav.lan:4533/rest/...`?"
debugging sessions.

## Wire types vs core types

Internal `WireArtist` / `WireAlbum` / `WireTrack` structs mirror
Subsonic's JSON shapes exactly and carry `From` impls into
`music-core`'s `Artist`, `Album`, `Track`. These per-field wire
structs stay private; what *is* public are the small grouping types
that bundle a converted result — `AlbumWithSongs`,
`ArtistWithAlbums`, `SearchResult3` (all re-exported from the crate
root, in `src/wire.rs`). The gateway proxy handler doesn't go through
this crate at all; it forwards the raw Subsonic JSON through
unchanged.

The track wire type now also carries the metadata added in the
play-counts work — `play_count`, `played` (last-played timestamp),
`genre`, `bit_rate`, `content_type`, `suffix`, `disc_number` — which
flow into the corresponding `music-core::Track` fields. `Album`
likewise gained `play_count` / `played`.

Why not just deserialize directly into `music-core` types? Because
Subsonic's responses are wrapped:

```json
{
  "subsonic-response": {
    "status": "ok",
    "version": "1.16.1",
    "albumList2": { "album": [ ... ] }
  }
}
```

Stripping the wrapper before exposing the inner array to callers is
this crate's whole job. The wire types model the wrapper; the
conversion into core types peels it off.

## Errors

`subsonic::Error` is the public error enum. Variants:
- `Transport(reqwest::Error)` — transport failure.
- `InvalidUrl(url::ParseError)` — bad base URL.
- `Subsonic { code, message }` — Navidrome returned a Subsonic error
  envelope. Codes are Subsonic-spec error codes (10 = required param
  missing, 40 = wrong username/password, etc.).
- `BadResponse(String)` — well-formed JSON missing an expected field
  (e.g. no `subsonic-response` key, or a `getAlbum` with no `album`).
- `Config(String)` — client misconfiguration surfaced at build time:
  an invalid bearer token, or a CA bundle that parses to zero certs.
- `Json(serde_json::Error)` — Navidrome returned non-JSON or
  malformed JSON.

`Error::subsonic_error()` returns `Some((code, message))` for the
`Subsonic` variant and `None` otherwise — the convenient way to
distinguish an API-level rejection from a transport failure. Callers
usually want `Subsonic { code: 40, .. }` to surface as "wrong
password" specifically; everything else is "we couldn't reach
Navidrome."

## Tests

`tests/` uses `wiremock` to stub Navidrome responses. Tests exercise:
- Auth header construction (correct `t = md5(password + salt)` value).
- URL join behaviour with and without trailing slash.
- Error envelope parsing (Subsonic returns 200 with an error in the
  body; our error path has to detect that).
- `stream_url` targets `/rest/stream` with the track id and full auth
  query string.
- `with_bearer` attaches the `Authorization` header; `with_tls`
  composes with it in either order, and a bad CA bundle is a
  `Config` error.
- The newer endpoints — `get_song`, `get_artists` (index
  flattening), `get_artist` (artist/albums split), `search3` (all
  three buckets, missing buckets, no-result-key).

35 tests total (24 integration tests in `tests/{auth,client,wire}.rs`
plus 11 unit tests in `src/`), all using `wiremock` or in-memory
fixtures. No real network.

## Notable design

The crate is small (~1.3k lines including tests) on purpose. Subsonic
is a sprawling API; we implement what we need and pass the rest
through at the gateway. That keeps this crate stable: changes to
Subsonic-the-spec don't invalidate code we never wrote.

## Future shape

The original plan compiled this crate to a UniFFI binding (`music-ffi`)
for a native Kotlin mobile app. That plan (P4) was **retired** — mobile
is the installable PWA, which talks to the gateway over HTTP and never
links this crate. So `music-ffi` will not be built; this crate stays a
CLI/gateway-side Rust dependency. The narrow public surface is still
worth keeping for readability and test cost.
