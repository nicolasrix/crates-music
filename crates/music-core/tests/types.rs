//! Round-trip tests for the high-level domain types. The wire format is
//! JSON because that's what every consumer (Subsonic, gateway, sync) speaks.

use music_core::{Album, AlbumId, Artist, ArtistId, Track, TrackId};

#[test]
fn artist_roundtrips() {
    let artist = Artist {
        id: ArtistId::from("ar-1"),
        name: "Brian Eno".to_string(),
        album_count: Some(42),
    };
    let json = serde_json::to_string(&artist).unwrap();
    let back: Artist = serde_json::from_str(&json).unwrap();
    assert_eq!(back, artist);
}

#[test]
fn album_roundtrips() {
    let album = Album {
        id: AlbumId::from("al-1"),
        name: "Music for Airports".to_string(),
        artist_name: Some("Brian Eno".to_string()),
        artist_id: Some(ArtistId::from("ar-1")),
        year: Some(1978),
        song_count: 4,
        duration_seconds: 2880,
        cover_art_id: Some("cover-1".to_string()),
    };
    let json = serde_json::to_string(&album).unwrap();
    let back: Album = serde_json::from_str(&json).unwrap();
    assert_eq!(back, album);
}

#[test]
fn track_roundtrips() {
    let track = Track {
        id: TrackId::from("t-1"),
        title: "1/1".to_string(),
        album_id: Some(AlbumId::from("al-1")),
        album_name: Some("Music for Airports".to_string()),
        artist_id: Some(ArtistId::from("ar-1")),
        artist_name: Some("Brian Eno".to_string()),
        track_number: Some(1),
        disc_number: Some(1),
        duration_seconds: Some(1042),
        bit_rate_kbps: Some(320),
        content_type: Some("audio/flac".to_string()),
        suffix: Some("flac".to_string()),
    };
    let json = serde_json::to_string(&track).unwrap();
    let back: Track = serde_json::from_str(&json).unwrap();
    assert_eq!(back, track);
}

#[test]
fn track_with_only_required_fields_roundtrips() {
    // Subsonic responses can omit nearly every field. Required: id, title.
    let json = r#"{"id":"t-1","title":"untitled"}"#;
    let track: Track = serde_json::from_str(json).unwrap();
    assert_eq!(track.id, TrackId::from("t-1"));
    assert_eq!(track.title, "untitled");
    assert!(track.album_id.is_none());
    assert!(track.duration_seconds.is_none());
}

#[test]
fn album_duration_helper_returns_std_duration() {
    let album = Album {
        id: AlbumId::from("al-1"),
        name: "x".into(),
        artist_name: None,
        artist_id: None,
        year: None,
        song_count: 0,
        duration_seconds: 90,
        cover_art_id: None,
    };
    assert_eq!(album.duration(), std::time::Duration::from_secs(90));
}

#[test]
fn track_duration_helper_returns_optional_std_duration() {
    let mut track = Track {
        id: TrackId::from("t-1"),
        title: "x".into(),
        album_id: None,
        album_name: None,
        artist_id: None,
        artist_name: None,
        track_number: None,
        disc_number: None,
        duration_seconds: Some(125),
        bit_rate_kbps: None,
        content_type: None,
        suffix: None,
    };
    assert_eq!(track.duration(), Some(std::time::Duration::from_secs(125)));
    track.duration_seconds = None;
    assert_eq!(track.duration(), None);
}
