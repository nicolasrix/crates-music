//! Round-trip tests for the high-level domain types. The wire format is
//! JSON because that's what every consumer (Subsonic, gateway, sync) speaks.

use music_core::{
    Album, AlbumId, Artist, ArtistId, PlaybackState, Queue, QueueItem, QueueItemId, SessionAnchor,
    SessionId, Track, TrackId,
};

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
fn empty_queue_default_is_empty() {
    let q = Queue::default();
    assert!(q.items.is_empty());
}

#[test]
fn queue_with_items_roundtrips() {
    let queue = Queue {
        items: vec![
            QueueItem {
                item_id: QueueItemId::from("qi-1"),
                track_id: TrackId::from("t-1"),
            },
            QueueItem {
                item_id: QueueItemId::from("qi-2"),
                track_id: TrackId::from("t-2"),
            },
        ],
    };
    let json = serde_json::to_string(&queue).unwrap();
    let back: Queue = serde_json::from_str(&json).unwrap();
    assert_eq!(back, queue);
}

#[test]
fn queue_item_id_serializes_as_plain_string() {
    let id = QueueItemId::from("qi-1");
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, "\"qi-1\"");
}

#[test]
fn playback_state_default_is_idle_empty() {
    let s = PlaybackState::default();
    assert!(s.queue.items.is_empty());
    assert_eq!(s.now_playing_index, None);
    assert_eq!(s.position_ms, 0);
    assert!(!s.is_playing);
    assert!(s.session_anchor.is_none());
}

#[test]
fn playback_state_roundtrips() {
    let state = PlaybackState {
        queue: Queue {
            items: vec![QueueItem {
                item_id: QueueItemId::from("qi-1"),
                track_id: TrackId::from("t-1"),
            }],
        },
        now_playing_index: Some(0),
        position_ms: 12_345,
        is_playing: true,
        session_anchor: None,
    };
    let json = serde_json::to_string(&state).unwrap();
    let back: PlaybackState = serde_json::from_str(&json).unwrap();
    assert_eq!(back, state);
}

#[test]
fn playback_state_with_session_anchor_roundtrips() {
    let state = PlaybackState {
        queue: Queue {
            items: vec![QueueItem {
                item_id: QueueItemId::from("qi-1"),
                track_id: TrackId::from("t-1"),
            }],
        },
        now_playing_index: Some(0),
        position_ms: 0,
        is_playing: true,
        session_anchor: Some(SessionAnchor {
            session_id: SessionId::from("0192a000-0000-7000-8000-000000000001"),
            track_id: TrackId::from("t-1"),
            started_ms: 1_710_000_000_000,
        }),
    };
    let json = serde_json::to_string(&state).unwrap();
    let back: PlaybackState = serde_json::from_str(&json).unwrap();
    assert_eq!(back, state);
}

#[test]
fn playback_state_without_anchor_omits_field_on_wire() {
    // session_anchor is None most of the time; we don't want every
    // snapshot to carry a `"session_anchor":null` payload.
    let state = PlaybackState::default();
    let json = serde_json::to_string(&state).unwrap();
    assert!(
        !json.contains("session_anchor"),
        "default PlaybackState should not serialize session_anchor field, got: {json}"
    );
}

#[test]
fn playback_state_deserializes_legacy_payload_without_session_anchor() {
    // Snapshots written before the field existed must still parse.
    let json = r#"{"queue":{"items":[]},"now_playing_index":null,"position_ms":0,"is_playing":false}"#;
    let state: PlaybackState = serde_json::from_str(json).unwrap();
    assert!(state.session_anchor.is_none());
}

#[test]
fn session_id_serializes_as_plain_string() {
    let id = SessionId::from("0192a000-0000-7000-8000-000000000001");
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, "\"0192a000-0000-7000-8000-000000000001\"");
}

#[test]
fn session_anchor_roundtrips() {
    let anchor = SessionAnchor {
        session_id: SessionId::from("sess-x"),
        track_id: TrackId::from("t-1"),
        started_ms: 1_710_000_000_000,
    };
    let json = serde_json::to_string(&anchor).unwrap();
    let back: SessionAnchor = serde_json::from_str(&json).unwrap();
    assert_eq!(back, anchor);
}

#[test]
fn playback_state_now_playing_helper_returns_track_or_none() {
    let mut state = PlaybackState {
        queue: Queue {
            items: vec![
                QueueItem {
                    item_id: QueueItemId::from("qi-1"),
                    track_id: TrackId::from("t-1"),
                },
                QueueItem {
                    item_id: QueueItemId::from("qi-2"),
                    track_id: TrackId::from("t-2"),
                },
            ],
        },
        now_playing_index: Some(1),
        position_ms: 0,
        is_playing: false,
        session_anchor: None,
    };
    assert_eq!(
        state.now_playing().map(|i| i.track_id.clone()),
        Some(TrackId::from("t-2"))
    );
    state.now_playing_index = None;
    assert!(state.now_playing().is_none());
    state.now_playing_index = Some(99);
    assert!(
        state.now_playing().is_none(),
        "out-of-bounds index returns None, never panics"
    );
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
