//! Tests for the Subsonic JSON wire format → music-core conversions.
//! No HTTP — just deserialize realistic response bodies and check the
//! conversion lands the right shape in our domain types.

use music_core::{AlbumId, ArtistId, TrackId};
use music_subsonic::wire;

#[test]
fn parses_album_list2_response() {
    let body = r#"{
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "type": "navidrome",
            "albumList2": {
                "album": [
                    {
                        "id": "al-1",
                        "name": "Music for Airports",
                        "artist": "Brian Eno",
                        "artistId": "ar-1",
                        "songCount": 4,
                        "duration": 2880,
                        "year": 1978,
                        "coverArt": "al-1"
                    },
                    {
                        "id": "al-2",
                        "name": "Apollo",
                        "artist": "Brian Eno",
                        "artistId": "ar-1",
                        "songCount": 12,
                        "duration": 2740
                    }
                ]
            }
        }
    }"#;

    let albums = wire::parse_album_list2(body).unwrap();
    assert_eq!(albums.len(), 2);

    let first = &albums[0];
    assert_eq!(first.id, AlbumId::from("al-1"));
    assert_eq!(first.name, "Music for Airports");
    assert_eq!(first.artist_name.as_deref(), Some("Brian Eno"));
    assert_eq!(first.artist_id, Some(ArtistId::from("ar-1")));
    assert_eq!(first.song_count, 4);
    assert_eq!(first.duration_seconds, 2880);
    assert_eq!(first.year, Some(1978));
    assert_eq!(first.cover_art_id.as_deref(), Some("al-1"));

    // Second album omits year and coverArt — should still parse.
    assert_eq!(albums[1].year, None);
    assert_eq!(albums[1].cover_art_id, None);
}

#[test]
fn parses_get_album_response_with_songs() {
    let body = r#"{
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "album": {
                "id": "al-1",
                "name": "Music for Airports",
                "artist": "Brian Eno",
                "artistId": "ar-1",
                "songCount": 2,
                "duration": 2880,
                "year": 1978,
                "song": [
                    {
                        "id": "t-1",
                        "title": "1/1",
                        "album": "Music for Airports",
                        "albumId": "al-1",
                        "artist": "Brian Eno",
                        "artistId": "ar-1",
                        "track": 1,
                        "discNumber": 1,
                        "duration": 1042,
                        "bitRate": 320,
                        "contentType": "audio/flac",
                        "suffix": "flac"
                    },
                    {
                        "id": "t-2",
                        "title": "2/1",
                        "duration": 514
                    }
                ]
            }
        }
    }"#;

    let album_with_songs = wire::parse_get_album(body).unwrap();
    assert_eq!(album_with_songs.album.id, AlbumId::from("al-1"));
    assert_eq!(album_with_songs.tracks.len(), 2);

    let first = &album_with_songs.tracks[0];
    assert_eq!(first.id, TrackId::from("t-1"));
    assert_eq!(first.title, "1/1");
    assert_eq!(first.album_id, Some(AlbumId::from("al-1")));
    assert_eq!(first.track_number, Some(1));
    assert_eq!(first.disc_number, Some(1));
    assert_eq!(first.duration_seconds, Some(1042));
    assert_eq!(first.bit_rate_kbps, Some(320));
    assert_eq!(first.content_type.as_deref(), Some("audio/flac"));

    // Sparse second song parses with mostly Nones.
    assert_eq!(album_with_songs.tracks[1].id, TrackId::from("t-2"));
    assert_eq!(album_with_songs.tracks[1].album_id, None);
}

#[test]
fn parses_get_song_response() {
    // Subsonic /rest/getSong returns the track under the `song` key, with
    // the same fields as one entry from `getAlbum.song[]`.
    let body = r#"{
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "song": {
                "id": "t-1",
                "title": "1/1",
                "album": "Music for Airports",
                "albumId": "al-1",
                "artist": "Brian Eno",
                "artistId": "ar-1",
                "track": 1,
                "discNumber": 1,
                "duration": 1042,
                "bitRate": 320,
                "contentType": "audio/flac",
                "suffix": "flac"
            }
        }
    }"#;

    let track = wire::parse_get_song(body).unwrap();
    assert_eq!(track.id, TrackId::from("t-1"));
    assert_eq!(track.title, "1/1");
    assert_eq!(track.album_id, Some(AlbumId::from("al-1")));
    assert_eq!(track.duration_seconds, Some(1042));
    assert_eq!(track.bit_rate_kbps, Some(320));
}

#[test]
fn parses_get_song_with_sparse_fields() {
    // Older Subsonic servers may omit duration / bitRate.
    let body = r#"{
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "song": {
                "id": "t-9",
                "title": "Untitled"
            }
        }
    }"#;

    let track = wire::parse_get_song(body).unwrap();
    assert_eq!(track.id, TrackId::from("t-9"));
    assert_eq!(track.duration_seconds, None);
}

#[test]
fn parses_error_envelope_into_typed_error() {
    let body = r#"{
        "subsonic-response": {
            "status": "failed",
            "version": "1.16.1",
            "error": {
                "code": 40,
                "message": "Wrong username or password."
            }
        }
    }"#;

    // Any parser sees the failed envelope before parsing the payload.
    let err = wire::parse_album_list2(body).unwrap_err();
    let (code, message) = err.subsonic_error().expect("expected subsonic error");
    assert_eq!(code, 40);
    assert_eq!(message, "Wrong username or password.");
}

#[test]
fn parses_play_count_and_played_on_song() {
    // OpenSubsonic extension fields. Navidrome emits both on getSong /
    // getAlbum responses; older Subsonic servers may omit them and the
    // parse must still succeed (covered by parses_get_song_with_sparse_fields).
    let body = r#"{
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "song": {
                "id": "t-1",
                "title": "1/1",
                "playCount": 17,
                "played": "2026-04-15T12:34:56Z"
            }
        }
    }"#;
    let track = wire::parse_get_song(body).unwrap();
    assert_eq!(track.play_count, Some(17));
    assert_eq!(track.played_at.as_deref(), Some("2026-04-15T12:34:56Z"));
}

#[test]
fn parses_genre_on_song() {
    // Subsonic emits `<song genre="Ambient">` on song elements when the
    // server has the tag. We need it on the wire so the recommender's
    // metadata cache can store it and the latent-space diagnostics page
    // can colour clusters by reported genre. Older / untagged songs
    // omit the field entirely — `song_without_play_count_or_played_parses_with_none`
    // already covers the null-genre case.
    let body = r#"{
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "song": {
                "id": "t-genre",
                "title": "1/1",
                "genre": "Ambient"
            }
        }
    }"#;
    let track = wire::parse_get_song(body).unwrap();
    assert_eq!(track.genre.as_deref(), Some("Ambient"));
}

#[test]
fn parses_year_on_song() {
    // Subsonic emits `<song year="…">` for tracks. We need it on the
    // wire so the recommender's track_metadata cache can store year and
    // the diagnostics panel can render "Album · 1978". Older servers
    // that omit it must still parse — covered by
    // parses_get_song_with_sparse_fields.
    let body = r#"{
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "song": {
                "id": "t-1",
                "title": "1/1",
                "year": 1978
            }
        }
    }"#;
    let track = wire::parse_get_song(body).unwrap();
    assert_eq!(track.year, Some(1978));
}

#[test]
fn song_without_play_count_or_played_parses_with_none() {
    // Older Subsonic servers — no OpenSubsonic extension. The fields
    // must default to None so existing libraries don't 422.
    let body = r#"{
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "song": { "id": "t-9", "title": "Untitled" }
        }
    }"#;
    let track = wire::parse_get_song(body).unwrap();
    assert_eq!(track.play_count, None);
    assert_eq!(track.played_at, None);
    // Genre defaults to None on servers that don't emit the tag.
    assert_eq!(track.genre, None);
}

#[test]
fn parses_play_count_and_played_on_album() {
    // Albums also surface playCount/played in the OpenSubsonic spec.
    // Navidrome derives album.playCount from track.playCount sums.
    let body = r#"{
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "album": {
                "id": "al-1",
                "name": "Music for Airports",
                "artist": "Brian Eno",
                "songCount": 0,
                "duration": 0,
                "playCount": 42,
                "played": "2026-05-01T10:00:00Z"
            }
        }
    }"#;
    let album_with_songs = wire::parse_get_album(body).unwrap();
    assert_eq!(album_with_songs.album.play_count, Some(42));
    assert_eq!(
        album_with_songs.album.played_at.as_deref(),
        Some("2026-05-01T10:00:00Z")
    );
}

#[test]
fn parses_empty_album_list_when_field_missing() {
    // Subsonic returns no `album` array when the list is empty.
    let body = r#"{
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "albumList2": {}
        }
    }"#;
    let albums = wire::parse_album_list2(body).unwrap();
    assert!(albums.is_empty());
}
