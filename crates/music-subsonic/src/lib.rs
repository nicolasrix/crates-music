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

#[derive(Clone, Debug)]
pub struct Client {
    base: Url,
    creds: Credentials,
    http: Http,
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
        let http = Http::builder().build()?;
        Ok(Self { base, creds, http })
    }

    /// Attach a bearer token sent as `Authorization: Bearer <token>` on every
    /// outgoing request. Used when the client targets the music-gateway
    /// (which authenticates via a shared bearer rather than Subsonic's
    /// token+salt scheme — though the latter is still appended and ignored
    /// downstream, since the gateway strips client-supplied auth params).
    pub fn with_bearer(mut self, bearer: &str) -> Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {bearer}"))
            .map_err(|e| Error::Config(format!("invalid bearer token: {e}")))?;
        headers.insert(reqwest::header::AUTHORIZATION, value);
        self.http = Http::builder().default_headers(headers).build()?;
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
        self.build_url("stream", &[("id", id.as_str().to_string())])
    }

    pub fn http(&self) -> &Http {
        &self.http
    }
}
