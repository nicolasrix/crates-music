# music-subsonic

**Path:** `crates/music-subsonic/`
**Type:** library
**Test count:** 16

Typed Subsonic / OpenSubsonic HTTP client. Implements only the
endpoints crates-music actually uses; not a general-purpose Subsonic
SDK.

## Scope

Currently implemented:

| Subsonic endpoint | Method | Returns |
|---|---|---|
| `/rest/ping` | `Client::ping` | `()` |
| `/rest/getAlbumList2` | `Client::album_list` | `Vec<Album>` |
| `/rest/getAlbum` | `Client::album` | `AlbumWithSongs` |
| `/rest/stream` | `Client::stream` | streaming `bytes::Bytes` |

Not implemented (but trivial to add by following the same pattern):
search, playlists, scrobble. The gateway proxies these via
pass-through (`/rest/*` → Navidrome), so clients can hit them
through Subsonic SDKs even though this crate doesn't model them.

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
    .album_list(AlbumListType::Newest, 100, 0)
    .await?;
```

For the gateway-proxy mode (CLI talking to gateway, not Navidrome):

```rust
let client = Client::with_bearer("https://gateway.local:8443", "<token>")?;
let albums = client
    .album_list(AlbumListType::Newest, 100, 0)
    .await?;
```

`with_bearer` swaps Subsonic's password+salt token (`md5(password+salt)`)
for an `Authorization: Bearer <token>` header. The gateway doesn't
care about the Subsonic auth scheme; bearer is what gates the proxy.

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

`wire::Album`, `wire::AlbumWithSongs`, `wire::Song` mirror Subsonic's
JSON shapes exactly. They have `From` impls into `music-core`'s
`Album`, `Track`, etc. The wire types stay private to this crate
unless a caller specifically wants the unconverted JSON shape (which
the gateway proxy handler does — it forwards the JSON through
unchanged).

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
- `Http(reqwest::Error)` — transport failure.
- `Subsonic { code, message }` — Navidrome returned a Subsonic error
  envelope. Codes are Subsonic-spec error codes (10 = required param
  missing, 40 = wrong username/password, etc.).
- `Json(serde_json::Error)` — Navidrome returned non-JSON or
  malformed JSON.

Callers usually want `Subsonic { code: 40, .. }` to surface as "wrong
password" specifically; everything else is "we couldn't reach
Navidrome."

## Tests

`tests/` uses `wiremock` to stub Navidrome responses. Tests exercise:
- Auth header construction (correct `t = md5(password + salt)` value).
- URL join behaviour with and without trailing slash.
- Error envelope parsing (Subsonic returns 200 with an error in the
  body; our error path has to detect that).
- Stream response — `stream()` returns `impl Stream<Item = Bytes>`,
  and the test consumes it.

16 tests, all using `wiremock`. No real network.

## Notable design

The crate is tiny (~600 lines including tests) on purpose. Subsonic
is a sprawling API; we implement what we need and pass the rest
through at the gateway. That keeps this crate stable: changes to
Subsonic-the-spec don't invalidate code we never wrote.

## Future shape

When mobile lands (P4), this crate gets compiled to a UniFFI binding
(`music-ffi`) so Kotlin can drive it without re-implementing Subsonic.
That's why the public surface is kept narrow — wide APIs are painful
through UniFFI.
