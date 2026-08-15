//! Gateway lyrics fetchers (`/v1/lyrics/*`).
//!
//! The gateway normalizes every source — Navidrome's own tags, LRCLIB,
//! synced or not — into one shape, so this client never parses an LRC file.
//! That is why the TUI and the web client can share a design without
//! sharing a line of parsing code.
//!
//! Three outcomes have to stay distinguishable all the way to the UI, which
//! is why this module returns a [`LyricsOutcome`] rather than mapping the
//! failures into [`ApiError`]:
//!
//! * a document whose `source` is `"none"` — the gateway *checked* and this
//!   track has no lyrics;
//! * [`LyricsOutcome::Unavailable`] (503) — nothing could be reached, so we
//!   do not know;
//! * [`LyricsOutcome::Disabled`] (404) — the feature is off on this gateway.
//!
//! Collapse the first two and a provider outage renders as a permanent
//! absence, which is exactly what the gateway's status codes exist to
//! prevent.

use anyhow::{Context, anyhow};
use reqwest::StatusCode;
use serde::Deserialize;
use url::Url;

use super::ApiError;
use crate::config::Config;
use crate::gateway::{endpoint, http_client, require_gateway};

/// One timed line. `start_ms` is relative to the start of the track and
/// already carries any provider-supplied offset.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LyricLine {
    pub start_ms: i64,
    pub text: String,
}

/// A resolved lyrics document. Extra wire fields (`provider_id`,
/// `fetched_at`) are ignored — nothing in the CLI renders them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LyricsDoc {
    pub track_id: String,
    /// `"navidrome"` / `"lrclib"` / `"none"`. Left as a string rather than
    /// an enum so a gateway that grows a fourth source doesn't fail the
    /// parse on an older CLI.
    pub source: String,
    /// How closely a provider lookup matched: `"exact"` / `"no_album"` /
    /// `"search"`. `"search"` is the fuzziest tier and the one worth
    /// offering a re-resolve for.
    #[serde(default)]
    pub match_kind: Option<String>,
    /// True when `lines` carries usable timings.
    pub synced: bool,
    pub instrumental: bool,
    /// `null` (not `[]`) when unsynced, so "no timings" cannot be mistaken
    /// for "timings, but empty".
    #[serde(default)]
    pub lines: Option<Vec<LyricLine>>,
    #[serde(default)]
    pub plain: Option<String>,
}

impl LyricsDoc {
    /// A confirmed absence: the gateway looked and there are none.
    pub fn is_absent(&self) -> bool {
        self.source == "none"
    }

    /// Where the words came from, for an attribution footer. `None` when
    /// there is nothing to attribute.
    pub fn attribution(&self) -> Option<&'static str> {
        match self.source.as_str() {
            "none" => None,
            "navidrome" => Some("from the file's own tags"),
            _ if self.match_kind.as_deref() == Some("search") => {
                Some("from lrclib.net — closest match by title and length")
            }
            _ => Some("from lrclib.net"),
        }
    }
}

/// What a lyrics request resolved to. See the module note for why the two
/// failure shapes are values here rather than errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LyricsOutcome {
    Doc(Box<LyricsDoc>),
    /// 503 — no lyrics source reachable *right now*. Worth retrying.
    Unavailable,
    /// 404 — lyrics are switched off on this gateway. Retrying will not
    /// help until an admin changes the config.
    Disabled,
}

async fn request(
    config: &Config,
    track_id: &str,
    force: bool,
) -> Result<LyricsOutcome, ApiError> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    // Built through `Url` rather than `format!` because a Navidrome track id
    // is opaque to us — `path_segments_mut().push()` percent-encodes it,
    // where interpolation would let a `/` or `?` in an id rewrite the route.
    let mut url = Url::parse(&endpoint(gw, "/v1/lyrics")).context("parsing gateway url")?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| anyhow!("gateway url cannot have a path"))?;
        segments.push(track_id);
        if force {
            segments.push("refresh");
        }
    }
    let url = String::from(url);
    let client = http_client(gw)?;
    let req = if force {
        client.post(&url)
    } else {
        client.get(&url)
    };

    let resp = req
        .bearer_auth(&token)
        .send()
        .await
        .context("requesting lyrics")?;
    match resp.status() {
        StatusCode::NOT_FOUND => Ok(LyricsOutcome::Disabled),
        StatusCode::SERVICE_UNAVAILABLE => Ok(LyricsOutcome::Unavailable),
        // Only the refresh can 403 (guests may read lyrics but not trigger
        // a re-resolution that rewrites a row the whole household reads).
        StatusCode::FORBIDDEN => Err(ApiError::Forbidden),
        status if status.is_success() => {
            let doc: LyricsDoc = resp.json().await.context("parsing lyrics")?;
            Ok(LyricsOutcome::Doc(Box::new(doc)))
        }
        status => {
            let text = resp.text().await.unwrap_or_default();
            Err(anyhow!("lyrics request failed ({status}): {text}").into())
        }
    }
}

/// `GET /v1/lyrics/:track_id` — whatever the gateway has, resolving it on
/// first ask.
pub async fn get_lyrics(config: &Config, track_id: &str) -> Result<LyricsOutcome, ApiError> {
    request(config, track_id, false).await
}

/// `POST /v1/lyrics/:track_id/refresh` — drop the cached answer and resolve
/// again. The escape hatch when a fuzzy match landed on the wrong song.
pub async fn refresh_lyrics(config: &Config, track_id: &str) -> Result<LyricsOutcome, ApiError> {
    request(config, track_id, true).await
}
