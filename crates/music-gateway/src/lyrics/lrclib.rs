//! Client for LRCLIB — a keyless, account-free community database of
//! synced lyrics (<https://lrclib.net>).
//!
//! Why this provider and not one of the better-known names: a measured
//! sample of this library came back **19 synced / 3 plain / 2 instrumental
//! / 1 miss out of 25** with no key and no account. The alternatives each
//! fail a hard requirement — the big commercial API has no legitimate free
//! tier (the "free" route in the wild is a leaked key), and the popular
//! scraping targets return *unsynced* text only, which cannot drive line
//! highlighting at all.
//!
//! Two endpoints are used:
//!
//! * `GET /api/get` — exact lookup by artist + title (+ album, + duration).
//!   **404 is a normal answer**, not an error: it means "no entry", which
//!   the resolver caches as a miss.
//! * `GET /api/search` — fuzzy fallback. Its results are *candidates*, not
//!   answers; the resolver applies a duration guard before believing one.
//!
//! Egress hygiene: the client owns its own `reqwest::Client` rather than
//! borrowing the proxy's, so the outbound User-Agent identifies this
//! project (LRCLIB asks for that) without relabelling every Navidrome
//! call, and so a slow provider can have a tighter timeout than upstream.

use std::time::Duration;

use serde::Deserialize;
use url::Url;

/// Failure talking to the provider. Deliberately has no "not found"
/// variant: absence is `Ok(None)` / `Ok(vec![])`, because the resolver
/// must be able to tell "the provider says no" (cacheable) from "we could
/// not ask" (never cacheable — see [`super::resolver`]).
#[derive(Debug, thiserror::Error)]
pub enum LrclibError {
    #[error("lrclib request failed: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("lrclib returned status {0}")]
    Status(u16),

    #[error("lrclib config: {0}")]
    Config(String),
}

/// One provider entry. `duration_seconds` is what the guard compares
/// against; `synced_lyrics` is the field that makes the feature possible.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LrclibHit {
    pub id: i64,
    #[serde(default)]
    pub track_name: String,
    #[serde(default)]
    pub artist_name: String,
    #[serde(default)]
    pub album_name: Option<String>,
    /// Seconds, fractional. Absent on some older entries.
    #[serde(default, rename = "duration")]
    pub duration_seconds: Option<f64>,
    #[serde(default)]
    pub instrumental: bool,
    #[serde(default)]
    pub plain_lyrics: Option<String>,
    /// LRC text. `None` (or empty) on a plain-only entry.
    #[serde(default)]
    pub synced_lyrics: Option<String>,
}

impl LrclibHit {
    /// Whether the entry carries anything at all. The provider does store
    /// rows with both bodies null (an instrumental, or a placeholder).
    #[must_use]
    pub fn has_content(&self) -> bool {
        self.instrumental
            || self.synced_lyrics.as_deref().is_some_and(|s| !s.trim().is_empty())
            || self.plain_lyrics.as_deref().is_some_and(|s| !s.trim().is_empty())
    }
}

#[derive(Clone, Debug)]
pub struct LrclibClient {
    http: reqwest::Client,
    base: Url,
}

impl LrclibClient {
    /// `base` is the provider root (e.g. `https://lrclib.net`).
    ///
    /// The workspace builds reqwest against the bundled webpki roots
    /// rather than the system store — fine here, since the provider is a
    /// public Let's Encrypt host. That constraint only bites on
    /// privately-signed internal hosts.
    pub fn new(base: &str, user_agent: &str, timeout: Duration) -> Result<Self, LrclibError> {
        let base = Url::parse(base).map_err(|e| LrclibError::Config(format!("bad url: {e}")))?;
        let http = reqwest::Client::builder()
            .user_agent(user_agent)
            .timeout(timeout)
            .build()?;
        Ok(Self { http, base })
    }

    /// Exact lookup. `Ok(None)` is the provider's 404 — "no such entry" —
    /// and is a cacheable answer.
    pub async fn get(
        &self,
        artist: &str,
        title: &str,
        album: Option<&str>,
        duration_seconds: Option<u32>,
    ) -> Result<Option<LrclibHit>, LrclibError> {
        let mut url = self
            .base
            .join("/api/get")
            .map_err(|e| LrclibError::Config(format!("bad path: {e}")))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("artist_name", artist);
            q.append_pair("track_name", title);
            if let Some(album) = album {
                q.append_pair("album_name", album);
            }
            if let Some(secs) = duration_seconds {
                q.append_pair("duration", &secs.to_string());
            }
        }

        let response = self.http.get(url).send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(LrclibError::Status(response.status().as_u16()));
        }
        let hit: LrclibHit = response.json().await?;
        Ok(Some(hit))
    }

    /// Fuzzy search. Returns candidates in the provider's own ranking;
    /// an empty vec is a legitimate "nothing matched".
    pub async fn search(&self, artist: &str, title: &str) -> Result<Vec<LrclibHit>, LrclibError> {
        let mut url = self
            .base
            .join("/api/search")
            .map_err(|e| LrclibError::Config(format!("bad path: {e}")))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("artist_name", artist);
            q.append_pair("track_name", title);
        }

        let response = self.http.get(url).send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        if !response.status().is_success() {
            return Err(LrclibError::Status(response.status().as_u16()));
        }
        let hits: Vec<LrclibHit> = response.json().await?;
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(base: &str) -> LrclibClient {
        LrclibClient::new(base, "crates-music-test", Duration::from_secs(5)).unwrap()
    }

    #[tokio::test]
    async fn get_parses_a_synced_hit() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/get"))
            .and(query_param("artist_name", "Boards of Canada"))
            .and(query_param("album_name", "Music Has the Right"))
            .and(query_param("duration", "301"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 4242,
                "trackName": "Roygbiv",
                "artistName": "Boards of Canada",
                "albumName": "Music Has the Right",
                "duration": 301.0,
                "instrumental": false,
                "plainLyrics": "words",
                "syncedLyrics": "[00:01.00]words"
            })))
            .mount(&server)
            .await;

        let hit = client(&server.uri())
            .get(
                "Boards of Canada",
                "Roygbiv",
                Some("Music Has the Right"),
                Some(301),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hit.id, 4242);
        assert_eq!(hit.synced_lyrics.as_deref(), Some("[00:01.00]words"));
        assert!(hit.has_content());
    }

    #[tokio::test]
    async fn get_404_is_a_clean_absence_not_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/get"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        // The distinction the resolver depends on: this is cacheable as a
        // miss, whereas an error must never be cached.
        let got = client(&server.uri()).get("A", "B", None, None).await.unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn server_error_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/get"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let err = client(&server.uri()).get("A", "B", None, None).await.unwrap_err();
        assert!(matches!(err, LrclibError::Status(503)));
    }

    #[tokio::test]
    async fn search_returns_candidates_and_tolerates_sparse_rows() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": 1, "trackName": "T", "artistName": "A", "duration": 180.0,
                 "syncedLyrics": "[00:01.00]a"},
                // Sparse row: no duration, no bodies, no album. Must parse.
                {"id": 2, "trackName": "T", "artistName": "A"}
            ])))
            .mount(&server)
            .await;

        let hits = client(&server.uri()).search("A", "T").await.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[1].duration_seconds, None);
        assert!(!hits[1].has_content());
    }

    #[tokio::test]
    async fn instrumental_counts_as_content() {
        // An instrumental has no bodies but IS a definitive answer.
        let hit = LrclibHit {
            id: 7,
            track_name: "T".into(),
            artist_name: "A".into(),
            album_name: None,
            duration_seconds: Some(120.0),
            instrumental: true,
            plain_lyrics: None,
            synced_lyrics: None,
        };
        assert!(hit.has_content());
    }
}
