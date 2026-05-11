use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ids::{AlbumId, ArtistId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Album {
    pub id: AlbumId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_id: Option<ArtistId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u16>,
    #[serde(default)]
    pub song_count: u32,
    #[serde(default)]
    pub duration_seconds: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_art_id: Option<String>,
    /// Sum of track-level play counts when surfaced by Navidrome.
    /// See `Track::play_count` for the same caveat on freshness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub play_count: Option<u32>,
    /// ISO8601 wall-clock timestamp of the most recent play across the
    /// album's tracks; raw from upstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub played_at: Option<String>,
}

impl Album {
    pub fn duration(&self) -> Duration {
        Duration::from_secs(u64::from(self.duration_seconds))
    }
}
