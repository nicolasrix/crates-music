//! Typed Subsonic / OpenSubsonic HTTP client.
//!
//! Implements only the endpoints `crates-music` actually uses today:
//! `ping`, `getAlbumList2`, `getAlbum`, and `stream` (URL-only — actual
//! byte streaming is left to the caller, since it's the consumer that
//! decides whether to pipe to a player or a cache).

pub mod auth;
mod error;
pub mod wire;

use music_core::{Album, AlbumId, Artist, ArtistId, Track, TrackId};
use reqwest::Client as Http;
use url::Url;

pub use error::{Error, Result};
pub use wire::{AlbumWithSongs, ArtistWithAlbums, SearchResult3};

const CLIENT_NAME: &str = "crates-music";
const PROTOCOL_VERSION: &str = "1.16.1";

#[derive(Clone, Debug)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AlbumListType {
    Newest,
    Recent,
    Frequent,
    Random,
    AlphabeticalByName,
    AlphabeticalByArtist,
}

impl AlbumListType {
    fn as_param(self) -> &'static str {
        match self {
            Self::Newest => "newest",
            Self::Recent => "recent",
            Self::Frequent => "frequent",
            Self::Random => "random",
            Self::AlphabeticalByName => "alphabeticalByName",
            Self::AlphabeticalByArtist => "alphabeticalByArtist",
        }
    }
}

/// TLS trust configuration for the underlying reqwest client. The workspace
/// builds reqwest with `rustls-tls`, whose trust anchors are the *bundled*
/// webpki roots — not the system store — so a private CA (e.g. an mkcert
/// `gateway.local` cert) is invisible unless added here explicitly.
#[derive(Clone, Debug, Default)]
struct TlsOptions {
    /// Extra CA roots as a PEM bundle, added on top of the built-in roots.
    ca_cert_pem: Option<Vec<u8>>,
    /// Disable certificate verification entirely (debug/throwaway only).
    insecure: bool,
}

/// Build the reqwest client from the full set of options. Called whenever
/// `bearer` or `tls` changes so the two compose regardless of builder-call
/// order (a naive per-method rebuild would clobber whichever ran first).
fn build_http(bearer: Option<&str>, tls: &TlsOptions) -> Result<Http> {
    let mut builder = Http::builder();
    if let Some(bearer) = bearer {
        let mut headers = reqwest::header::HeaderMap::new();
        let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {bearer}"))
            .map_err(|e| Error::Config(format!("invalid bearer token: {e}")))?;
        headers.insert(reqwest::header::AUTHORIZATION, value);
        builder = builder.default_headers(headers);
    }
    if let Some(pem) = &tls.ca_cert_pem {
        let certs = reqwest::Certificate::from_pem_bundle(pem)
            .map_err(|e| Error::Config(format!("invalid CA certificate bundle: {e}")))?;
        // `from_pem_bundle` returns an empty vec (not an error) when the input
        // contains no PEM blocks — e.g. a misnamed or empty file. Adding zero
        // roots would silently leave the cert untrusted and surface as a
        // baffling `UnknownIssuer` at request time, so fail loudly instead.
        if certs.is_empty() {
            return Err(Error::Config(
                "CA certificate bundle contained no certificates".to_string(),
            ));
        }
        for cert in certs {
            builder = builder.add_root_certificate(cert);
        }
    }
    if tls.insecure {
        builder = builder.danger_accept_invalid_certs(true);
    }
    Ok(builder.build()?)
}

#[derive(Clone, Debug)]
pub struct Client {
    base: Url,
    creds: Credentials,
    http: Http,
    bearer: Option<String>,
    tls: TlsOptions,
}

impl Client {
    pub fn new(base: &str, creds: Credentials) -> Result<Self> {
        // Allow callers to pass either "http://host" or "http://host/" — Url::join
        // requires a trailing slash to keep the host as the base.
        let normalized = if base.ends_with('/') {
            base.to_string()
        } else {
            format!("{base}/")
        };
        let base = Url::parse(&normalized)?;
        let tls = TlsOptions::default();
        let http = build_http(None, &tls)?;
        Ok(Self {
            base,
            creds,
            http,
            bearer: None,
            tls,
        })
    }

    /// Attach a bearer token sent as `Authorization: Bearer <token>` on every
    /// outgoing request. Used when the client targets the music-gateway
    /// (which authenticates via a shared bearer rather than Subsonic's
    /// token+salt scheme — though the latter is still appended and ignored
    /// downstream, since the gateway strips client-supplied auth params).
    ///
    /// Composes with [`with_tls`](Self::with_tls) regardless of call order.
    pub fn with_bearer(mut self, bearer: &str) -> Result<Self> {
        self.bearer = Some(bearer.to_string());
        self.http = build_http(self.bearer.as_deref(), &self.tls)?;
        Ok(self)
    }

    /// Configure TLS trust. `ca_cert_pem` is an optional PEM bundle added as
    /// an *extra* root (e.g. the mkcert CA) so the client can verify a
    /// private `gateway.local` cert the built-in webpki roots don't know;
    /// `insecure` disables verification entirely. Mirrors the gateway HTTP
    /// client's `[gateway].ca_cert_path` / `insecure_tls` knobs so gateway-mode
    /// browse and recommend title-resolution verify the same cert the
    /// ratings/sync paths already do. Composes with
    /// [`with_bearer`](Self::with_bearer) regardless of call order.
    pub fn with_tls(mut self, ca_cert_pem: Option<&[u8]>, insecure: bool) -> Result<Self> {
        self.tls = TlsOptions {
            ca_cert_pem: ca_cert_pem.map(<[u8]>::to_vec),
            insecure,
        };
        self.http = build_http(self.bearer.as_deref(), &self.tls)?;
        Ok(self)
    }

    fn build_url(&self, method: &str, params: &[(&str, String)]) -> Result<Url> {
        let mut url = self.base.join(&format!("rest/{method}"))?;
        let salt = auth::random_salt();
        let token = auth::compute_token(&self.creds.password, &salt);
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("u", &self.creds.username)
                .append_pair("t", &token)
                .append_pair("s", &salt)
                .append_pair("v", PROTOCOL_VERSION)
                .append_pair("c", CLIENT_NAME)
                .append_pair("f", "json");
            for (k, v) in params {
                q.append_pair(k, v);
            }
        }
        Ok(url)
    }

    async fn fetch_text(&self, method: &str, params: &[(&str, String)]) -> Result<String> {
        let url = self.build_url(method, params)?;
        let res = self.http.get(url).send().await?.error_for_status()?;
        Ok(res.text().await?)
    }

    pub async fn ping(&self) -> Result<()> {
        let body = self.fetch_text("ping", &[]).await?;
        wire::parse_ping(&body)
    }

    /// Report a play (`scrobble`). `submission = false` is the "now
    /// playing" hint; `true` submits the play to the server's counts (and
    /// whatever scrobble targets it forwards to). The response carries no
    /// payload — only the envelope status matters.
    pub async fn scrobble(&self, id: &TrackId, submission: bool) -> Result<()> {
        let params = vec![
            ("id", id.as_str().to_string()),
            ("submission", submission.to_string()),
        ];
        let body = self.fetch_text("scrobble", &params).await?;
        wire::parse_ping(&body)
    }

    pub async fn get_album_list2(
        &self,
        list_type: AlbumListType,
        size: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Vec<Album>> {
        let mut params: Vec<(&str, String)> = vec![("type", list_type.as_param().to_string())];
        if let Some(size) = size {
            params.push(("size", size.to_string()));
        }
        if let Some(offset) = offset {
            params.push(("offset", offset.to_string()));
        }
        let body = self.fetch_text("getAlbumList2", &params).await?;
        wire::parse_album_list2(&body)
    }

    pub async fn get_album(&self, id: &AlbumId) -> Result<AlbumWithSongs> {
        let params = vec![("id", id.as_str().to_string())];
        let body = self.fetch_text("getAlbum", &params).await?;
        wire::parse_get_album(&body)
    }

    /// Fetch a single track's metadata by id. Used by the ingest pipeline
    /// to read `duration_seconds` so it can pick a `timeOffset` for the
    /// embedding window.
    pub async fn get_song(&self, id: &TrackId) -> Result<Track> {
        let params = vec![("id", id.as_str().to_string())];
        let body = self.fetch_text("getSong", &params).await?;
        wire::parse_get_song(&body)
    }

    /// List every artist (ID3 `getArtists`), flattened across the index
    /// buckets the server groups them into. Order is the server's
    /// (alphabetical).
    pub async fn get_artists(&self) -> Result<Vec<Artist>> {
        let body = self.fetch_text("getArtists", &[]).await?;
        wire::parse_get_artists(&body)
    }

    /// Fetch one artist with their albums (ID3 `getArtist`).
    pub async fn get_artist(&self, id: &ArtistId) -> Result<ArtistWithAlbums> {
        let params = vec![("id", id.as_str().to_string())];
        let body = self.fetch_text("getArtist", &params).await?;
        wire::parse_get_artist(&body)
    }

    /// The artist's most-played tracks (`getTopSongs`, keyed by artist
    /// *name*, not id — that's the Subsonic contract). `count` caps the
    /// result. Empty when the server has no play data for the artist.
    pub async fn get_top_songs(&self, artist_name: &str, count: u32) -> Result<Vec<Track>> {
        let params = vec![
            ("artist", artist_name.to_string()),
            ("count", count.to_string()),
        ];
        let body = self.fetch_text("getTopSongs", &params).await?;
        wire::parse_top_songs(&body)
    }

    /// Search across artists, albums and tracks (`search3`, ID3). `count`
    /// caps each bucket; `offset` pages within each bucket (Subsonic applies
    /// the same offset to all three). An empty `query` matches everything on
    /// Navidrome — used to page the full track list.
    pub async fn search3(
        &self,
        query: &str,
        count: u32,
        offset: u32,
    ) -> Result<SearchResult3> {
        let count = count.to_string();
        let offset = offset.to_string();
        let params = vec![
            ("query", query.to_string()),
            ("artistCount", count.clone()),
            ("artistOffset", offset.clone()),
            ("albumCount", count.clone()),
            ("albumOffset", offset.clone()),
            ("songCount", count),
            ("songOffset", offset),
        ];
        let body = self.fetch_text("search3", &params).await?;
        wire::parse_search3(&body)
    }

    /// Build the URL for streaming a track. The caller is responsible for
    /// performing the GET (potentially with `Range` headers) — this lets
    /// the caller wire bytes directly to a decoder, a cache, or both.
    pub fn stream_url(&self, id: &TrackId) -> Result<Url> {
        self.stream_url_with(id, None, None)
    }

    /// [`Self::stream_url`] with optional transcode hints. `format` +
    /// `max_bitrate` map to the Subsonic `format`/`maxBitRate` params;
    /// Navidrome transcodes on demand (`None`/`None` streams the original).
    pub fn stream_url_with(
        &self,
        id: &TrackId,
        format: Option<&str>,
        max_bitrate: Option<u32>,
    ) -> Result<Url> {
        let mut params = vec![("id", id.as_str().to_string())];
        if let Some(f) = format {
            params.push(("format", f.to_string()));
        }
        if let Some(b) = max_bitrate {
            params.push(("maxBitRate", b.to_string()));
        }
        self.build_url("stream", &params)
    }

    pub fn http(&self) -> &Http {
        &self.http
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> Credentials {
        Credentials {
            username: "u".to_string(),
            password: "p".to_string(),
        }
    }

    // A real self-signed cert exercises the parse + add-root path; we never
    // connect, so its (un)trustedness and expiry are irrelevant — only the
    // PEM/DER parse runs here.
    const SELF_SIGNED_PEM: &[u8] = b"-----BEGIN CERTIFICATE-----\n\
MIIBcjCCARmgAwIBAgIUJAOw160sBu1wvFyZKr/5gyd/lv0wCgYIKoZIzj0EAwIw\n\
DzENMAsGA1UEAwwEdGVzdDAeFw0yNjA2MDYwOTAyMDBaFw0yNjA2MDcwOTAyMDBa\n\
MA8xDTALBgNVBAMMBHRlc3QwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAARONpEF\n\
dgtGfv+7PjpMnsoDR1WpyVdikkgkKcolq9k2NCXXj7eFolQbQXZSJn0C1Gl8LM2g\n\
Oe7AUSWjJnVddC0ao1MwUTAdBgNVHQ4EFgQU+3fPClTbEIVHTxWtBx1zgHHkyBMw\n\
HwYDVR0jBBgwFoAU+3fPClTbEIVHTxWtBx1zgHHkyBMwDwYDVR0TAQH/BAUwAwEB\n\
/zAKBggqhkjOPQQDAgNHADBEAiBFIW5KsOxunxTnFj+sYyrZ9nS4qhJLKRibecy5\n\
oy+R+AIgLIyIB1tVYb30R48ES93LoP8uEOs60AKpuNtzl1feTuM=\n\
-----END CERTIFICATE-----\n";

    #[test]
    fn bearer_and_tls_compose_either_order() {
        // Whichever builder method runs last must not clobber the other's
        // effect — both orderings must succeed and produce a usable client.
        let bearer_first = Client::new("https://gateway.local:8443", creds())
            .unwrap()
            .with_bearer("tok")
            .unwrap()
            .with_tls(None, true)
            .unwrap();
        assert!(bearer_first.bearer.as_deref() == Some("tok"));
        assert!(bearer_first.tls.insecure);

        let tls_first = Client::new("https://gateway.local:8443", creds())
            .unwrap()
            .with_tls(None, true)
            .unwrap()
            .with_bearer("tok")
            .unwrap();
        assert!(tls_first.bearer.as_deref() == Some("tok"));
        assert!(tls_first.tls.insecure);
    }

    #[test]
    fn invalid_ca_bundle_is_a_config_error() {
        let err = Client::new("https://gateway.local:8443", creds())
            .unwrap()
            .with_tls(Some(b"not a pem"), false)
            .unwrap_err();
        assert!(matches!(err, Error::Config(_)), "got {err:?}");
    }

    #[test]
    fn ca_pem_bundle_is_accepted() {
        let client = Client::new("https://gateway.local:8443", creds())
            .unwrap()
            .with_tls(Some(SELF_SIGNED_PEM), false)
            .unwrap();
        assert!(client.tls.ca_cert_pem.is_some());
    }
}
