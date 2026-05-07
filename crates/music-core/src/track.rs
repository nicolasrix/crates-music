use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ids::{AlbumId, ArtistId, TrackId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_id: Option<AlbumId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_id: Option<ArtistId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disc_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_rate_kbps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
}

impl Track {
    pub fn duration(&self) -> Option<Duration> {
        self.duration_seconds
            .map(|s| Duration::from_secs(u64::from(s)))
    }
}
