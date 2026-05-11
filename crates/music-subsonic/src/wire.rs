//! Wire-level types for Subsonic JSON responses, plus conversions into
//! `music-core` domain types.
//!
//! Subsonic wraps every response in `{"subsonic-response": { ... }}` with
//! a `status` of `"ok"` or `"failed"`. We strip that envelope first, then
//! deserialize the method-specific payload.

use music_core::{Album, AlbumId, ArtistId, Track, TrackId};
use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumWithSongs {
    pub album: Album,
    pub tracks: Vec<Track>,
}

/// Strip the `subsonic-response` envelope and surface API-level errors.
fn unwrap_envelope(body: &str) -> Result<Value> {
    let raw: Value = serde_json::from_str(body)?;
    let inner = raw
        .get("subsonic-response")
        .cloned()
        .ok_or_else(|| Error::BadResponse("missing 'subsonic-response' key".into()))?;

    let status = inner.get("status").and_then(Value::as_str).unwrap_or("");
    if status != "ok" {
        let err = inner.get("error").cloned().unwrap_or(Value::Null);
        let code = err
            .get("code")
            .and_then(Value::as_i64)
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(0);
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        return Err(Error::Subsonic { code, message });
    }
    Ok(inner)
}

pub fn parse_ping(body: &str) -> Result<()> {
    unwrap_envelope(body).map(|_| ())
}

pub fn parse_album_list2(body: &str) -> Result<Vec<Album>> {
    let inner = unwrap_envelope(body)?;
    let Some(albums) = inner
        .get("albumList2")
        .and_then(|al| al.get("album"))
        .cloned()
    else {
        return Ok(Vec::new());
    };
    let wire: Vec<WireAlbum> = serde_json::from_value(albums)?;
    Ok(wire.into_iter().map(Into::into).collect())
}

pub fn parse_get_song(body: &str) -> Result<Track> {
    let inner = unwrap_envelope(body)?;
    let song_val = inner
        .get("song")
        .cloned()
        .ok_or_else(|| Error::BadResponse("missing 'song' field".into()))?;
    let wire: WireTrack = serde_json::from_value(song_val)?;
    Ok(wire.into())
}

pub fn parse_get_album(body: &str) -> Result<AlbumWithSongs> {
    let inner = unwrap_envelope(body)?;
    let mut album_val = inner
        .get("album")
        .cloned()
        .ok_or_else(|| Error::BadResponse("missing 'album' field".into()))?;

    let songs_val = album_val
        .as_object_mut()
        .and_then(|m| m.remove("song"))
        .unwrap_or(Value::Null);

    let wire_album: WireAlbum = serde_json::from_value(album_val)?;
    let album: Album = wire_album.into();

    let tracks: Vec<Track> = if songs_val.is_null() {
        Vec::new()
    } else {
        let wire_tracks: Vec<WireTrack> = serde_json::from_value(songs_val)?;
        wire_tracks.into_iter().map(Into::into).collect()
    };

    Ok(AlbumWithSongs { album, tracks })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireAlbum {
    id: String,
    name: String,
    artist: Option<String>,
    artist_id: Option<String>,
    cover_art: Option<String>,
    #[serde(default)]
    song_count: u32,
    #[serde(default)]
    duration: u32,
    year: Option<u16>,
    play_count: Option<u32>,
    // Subsonic emits `played` as a single word; serde's camelCase rename
    // leaves single-word fields unchanged, so this maps 1:1.
    played: Option<String>,
}

impl From<WireAlbum> for Album {
    fn from(w: WireAlbum) -> Self {
        Self {
            id: AlbumId::from(w.id),
            name: w.name,
            artist_name: w.artist,
            artist_id: w.artist_id.map(ArtistId::from),
            year: w.year,
            song_count: w.song_count,
            duration_seconds: w.duration,
            cover_art_id: w.cover_art,
            play_count: w.play_count,
            played_at: w.played,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTrack {
    id: String,
    title: String,
    album: Option<String>,
    album_id: Option<String>,
    artist: Option<String>,
    artist_id: Option<String>,
    track: Option<u32>,
    disc_number: Option<u32>,
    duration: Option<u32>,
    bit_rate: Option<u32>,
    content_type: Option<String>,
    suffix: Option<String>,
    year: Option<u16>,
    play_count: Option<u32>,
    played: Option<String>,
    genre: Option<String>,
}

impl From<WireTrack> for Track {
    fn from(w: WireTrack) -> Self {
        Self {
            id: TrackId::from(w.id),
            title: w.title,
            album_id: w.album_id.map(AlbumId::from),
            album_name: w.album,
            artist_id: w.artist_id.map(ArtistId::from),
            artist_name: w.artist,
            track_number: w.track,
            disc_number: w.disc_number,
            duration_seconds: w.duration,
            bit_rate_kbps: w.bit_rate,
            content_type: w.content_type,
            suffix: w.suffix,
            year: w.year,
            play_count: w.play_count,
            played_at: w.played,
            genre: w.genre,
        }
    }
}
