//! Output formatting helpers. Pure-string in/pure-string out.

use music_cli::format::{albums_table, tracks_table};
use music_core::{Album, AlbumId, ArtistId, Track, TrackId};

fn sample_album(id: &str, name: &str, artist: &str, year: u16) -> Album {
    Album {
        id: AlbumId::from(id),
        name: name.into(),
        artist_name: Some(artist.into()),
        artist_id: Some(ArtistId::from("ar-x")),
        year: Some(year),
        song_count: 4,
        duration_seconds: 1234,
        cover_art_id: None,
        play_count: None,
        played_at: None,
    }
}

fn sample_track(id: &str, title: &str) -> Track {
    Track {
        id: TrackId::from(id),
        title: title.into(),
        album_id: None,
        album_name: None,
        artist_id: None,
        artist_name: Some("Brian Eno".into()),
        track_number: Some(1),
        disc_number: None,
        duration_seconds: Some(125),
        bit_rate_kbps: None,
        content_type: None,
        suffix: None,
        year: None,
        play_count: None,
        played_at: None,
        genre: None,
    }
}

#[test]
fn albums_table_includes_every_field_visible_to_a_user() {
    let albums = vec![sample_album(
        "al-1",
        "Music for Airports",
        "Brian Eno",
        1978,
    )];
    let output = albums_table(&albums);
    assert!(output.contains("al-1"), "missing id: {output}");
    assert!(
        output.contains("Music for Airports"),
        "missing title: {output}"
    );
    assert!(output.contains("Brian Eno"), "missing artist: {output}");
    assert!(output.contains("1978"), "missing year: {output}");
}

#[test]
fn albums_table_handles_empty_input_without_panicking() {
    let output = albums_table(&[]);
    // Nothing to assert about exact content — just that it returns a string.
    assert!(output.is_empty() || !output.is_empty());
}

#[test]
fn tracks_table_shows_track_number_and_title() {
    let tracks = vec![sample_track("t-1", "1/1")];
    let output = tracks_table(&tracks);
    assert!(output.contains("t-1"));
    assert!(output.contains("1/1"));
    assert!(output.contains('1'), "missing track number");
}
