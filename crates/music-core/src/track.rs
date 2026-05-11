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
    /// Release year. Subsonic emits this on `<song>` elements; older
    /// servers may omit it, so it stays optional. Stored as `u16`
    /// because four-digit years comfortably fit and we want to
    /// disallow nonsense like negative or far-future values at the
    /// type level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u16>,
    /// Subsonic genre tag, free-form text. May be `None` either because
    /// the server omits it (older Subsonic builds, untagged libraries)
    /// or because the track has no genre metadata. Treated as an opaque
    /// string — we don't normalize or canonicalize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    /// Total number of times this track has been played.
    /// OpenSubsonic extension; older servers omit it and we surface
    /// `None`. The displayed source-of-truth lives upstream (Navidrome);
    /// the recommender's recency penalty reads from the gateway's
    /// `play_history` table instead, since this field is only refreshed
    /// when an upstream getSong/getAlbum lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub play_count: Option<u32>,
    /// ISO8601 wall-clock timestamp of the most recent play, raw from
    /// upstream. Stored as a string so we don't have to commit to a
    /// chrono / time-crate dependency in `music-core`; consumers parse
    /// or pass through to UI as-is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub played_at: Option<String>,
}

impl Track {
    pub fn duration(&self) -> Option<Duration> {
        self.duration_seconds
            .map(|s| Duration::from_secs(u64::from(s)))
    }
}
