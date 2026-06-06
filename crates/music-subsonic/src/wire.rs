//! Wire-level types for Subsonic JSON responses, plus conversions into
//! `music-core` domain types.
//!
//! Subsonic wraps every response in `{"subsonic-response": { ... }}` with
//! a `status` of `"ok"` or `"failed"`. We strip that envelope first, then
//! deserialize the method-specific payload.

use music_core::{Album, AlbumId, Artist, ArtistId, Track, TrackId};
use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumWithSongs {
    pub album: Album,
    pub tracks: Vec<Track>,
}

/// An artist plus their albums — the `getArtist` (ID3) payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistWithAlbums {
    pub artist: Artist,
    pub albums: Vec<Album>,
}

/// The three result buckets of `search3`. Any bucket may be empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchResult3 {
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
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

/// Flatten `getArtists` → `artists.index[].artist[]` into a flat list,
/// preserving the server's (already alphabetical) order.
pub fn parse_get_artists(body: &str) -> Result<Vec<Artist>> {
    let inner = unwrap_envelope(body)?;
    let Some(indexes) = inner
        .get("artists")
        .and_then(|a| a.get("index"))
        .and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for index in indexes {
        let Some(artists) = index.get("artist").cloned() else {
            continue;
        };
        let wire: Vec<WireArtist> = serde_json::from_value(artists)?;
        out.extend(wire.into_iter().map(Into::into));
    }
    Ok(out)
}

/// Parse `getArtist` (ID3): the artist plus their albums under `album`.
pub fn parse_get_artist(body: &str) -> Result<ArtistWithAlbums> {
    let inner = unwrap_envelope(body)?;
    let mut artist_val = inner
        .get("artist")
        .cloned()
        .ok_or_else(|| Error::BadResponse("missing 'artist' field".into()))?;

    let albums_val = artist_val
        .as_object_mut()
        .and_then(|m| m.remove("album"))
        .unwrap_or(Value::Null);

    let wire_artist: WireArtist = serde_json::from_value(artist_val)?;
    let artist: Artist = wire_artist.into();

    let albums: Vec<Album> = if albums_val.is_null() {
        Vec::new()
    } else {
        let wire_albums: Vec<WireAlbum> = serde_json::from_value(albums_val)?;
        wire_albums.into_iter().map(Into::into).collect()
    };

    Ok(ArtistWithAlbums { artist, albums })
}

/// Parse `search3` into its three buckets. Missing buckets are empty —
/// a query that matches only songs still returns `Ok`.
pub fn parse_search3(body: &str) -> Result<SearchResult3> {
    let inner = unwrap_envelope(body)?;
    let Some(result) = inner.get("searchResult3") else {
        return Ok(SearchResult3::default());
    };

    let artists = match result.get("artist").cloned() {
        Some(v) => serde_json::from_value::<Vec<WireArtist>>(v)?
            .into_iter()
            .map(Into::into)
            .collect(),
        None => Vec::new(),
    };
    let albums = match result.get("album").cloned() {
        Some(v) => serde_json::from_value::<Vec<WireAlbum>>(v)?
            .into_iter()
            .map(Into::into)
            .collect(),
        None => Vec::new(),
    };
    let tracks = match result.get("song").cloned() {
        Some(v) => serde_json::from_value::<Vec<WireTrack>>(v)?
            .into_iter()
            .map(Into::into)
            .collect(),
        None => Vec::new(),
    };

    Ok(SearchResult3 {
        artists,
        albums,
        tracks,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireArtist {
    id: String,
    name: String,
    album_count: Option<u32>,
}

impl From<WireArtist> for Artist {
    fn from(w: WireArtist) -> Self {
        Self {
            id: ArtistId::from(w.id),
            name: w.name,
            album_count: w.album_count,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(payload: &str) -> String {
        format!(r#"{{"subsonic-response":{{"status":"ok",{payload}}}}}"#)
    }

    #[test]
    fn get_artists_flattens_indexes_in_order() {
        let body = ok(
            r#""artists":{"index":[
                {"name":"A","artist":[
                    {"id":"ar-1","name":"Aphex Twin","albumCount":12},
                    {"id":"ar-2","name":"Autechre"}
                ]},
                {"name":"B","artist":[
                    {"id":"ar-3","name":"Boards of Canada","albumCount":7}
                ]}
            ]}"#,
        );
        let artists = parse_get_artists(&body).unwrap();
        let ids: Vec<&str> = artists.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["ar-1", "ar-2", "ar-3"]);
        assert_eq!(artists[0].album_count, Some(12));
        // Missing albumCount stays None rather than defaulting to 0.
        assert_eq!(artists[1].album_count, None);
    }

    #[test]
    fn get_artists_empty_is_ok() {
        assert!(parse_get_artists(&ok(r#""artists":{"index":[]}"#))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn get_artist_splits_artist_from_albums() {
        let body = ok(
            r#""artist":{"id":"ar-1","name":"Aphex Twin","albumCount":2,"album":[
                {"id":"al-1","name":"Drukqs","artist":"Aphex Twin","songCount":30},
                {"id":"al-2","name":"Syro","artist":"Aphex Twin","songCount":12}
            ]}"#,
        );
        let got = parse_get_artist(&body).unwrap();
        assert_eq!(got.artist.id.as_str(), "ar-1");
        assert_eq!(got.artist.album_count, Some(2));
        assert_eq!(got.albums.len(), 2);
        assert_eq!(got.albums[0].name, "Drukqs");
    }

    #[test]
    fn get_artist_without_albums_is_ok() {
        let got = parse_get_artist(&ok(r#""artist":{"id":"ar-9","name":"Obscure"}"#)).unwrap();
        assert!(got.albums.is_empty());
        assert_eq!(got.artist.name, "Obscure");
    }

    #[test]
    fn search3_fills_all_three_buckets() {
        let body = ok(
            r#""searchResult3":{
                "artist":[{"id":"ar-1","name":"Radiohead"}],
                "album":[{"id":"al-1","name":"Kid A","artist":"Radiohead","songCount":11}],
                "song":[{"id":"tr-1","title":"Idioteque","duration":323}]
            }"#,
        );
        let r = parse_search3(&body).unwrap();
        assert_eq!(r.artists.len(), 1);
        assert_eq!(r.albums.len(), 1);
        assert_eq!(r.tracks.len(), 1);
        assert_eq!(r.tracks[0].title, "Idioteque");
    }

    #[test]
    fn search3_missing_buckets_are_empty() {
        // A song-only match: artist/album keys absent entirely.
        let body = ok(r#""searchResult3":{"song":[{"id":"tr-1","title":"Lonely Hit"}]}"#);
        let r = parse_search3(&body).unwrap();
        assert!(r.artists.is_empty());
        assert!(r.albums.is_empty());
        assert_eq!(r.tracks.len(), 1);
    }

    #[test]
    fn search3_no_result_key_is_default() {
        assert_eq!(parse_search3(&ok(r#""x":1"#)).unwrap(), SearchResult3::default());
    }

    #[test]
    fn api_error_propagates() {
        let body = r#"{"subsonic-response":{"status":"failed","error":{"code":70,"message":"not found"}}}"#;
        let err = parse_get_artist(body).unwrap_err();
        assert!(matches!(err, Error::Subsonic { code: 70, .. }));
    }
}
