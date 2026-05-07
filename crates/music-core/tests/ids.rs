//! IDs are opaque newtypes around String. They must:
//!   - serialize to/from a JSON string (NOT an object — they are scalars on the wire)
//!   - be cheaply cloneable, hashable, and orderable for use as map keys
//!   - Display as the raw underlying ID
//!   - construct from any `Into<String>`

use music_core::{AlbumId, ArtistId, TrackId};

#[test]
fn track_id_roundtrips_as_json_string() {
    let id = TrackId::from("abc123");
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, r#""abc123""#);

    let back: TrackId = serde_json::from_str(&json).unwrap();
    assert_eq!(back, id);
}

#[test]
fn album_id_roundtrips_as_json_string() {
    let id = AlbumId::from("al-42");
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, r#""al-42""#);
    assert_eq!(serde_json::from_str::<AlbumId>(&json).unwrap(), id);
}

#[test]
fn artist_id_roundtrips_as_json_string() {
    let id = ArtistId::from("ar-7");
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, r#""ar-7""#);
    assert_eq!(serde_json::from_str::<ArtistId>(&json).unwrap(), id);
}

#[test]
fn ids_display_as_raw_value() {
    assert_eq!(format!("{}", TrackId::from("t1")), "t1");
    assert_eq!(format!("{}", AlbumId::from("a1")), "a1");
    assert_eq!(format!("{}", ArtistId::from("ar1")), "ar1");
}

#[test]
fn ids_implement_as_str() {
    let id = TrackId::from("xyz");
    assert_eq!(id.as_str(), "xyz");
}

#[test]
fn ids_are_distinct_types() {
    // Compile-time guarantee: TrackId and AlbumId cannot be confused.
    // (If this compiled with `let _: TrackId = AlbumId::from("x");` the test design is wrong.)
    let t = TrackId::from("a");
    let a = AlbumId::from("a");
    // Both have value "a" but they're different types.
    assert_eq!(t.as_str(), a.as_str());
}

#[test]
fn ids_are_hashable_and_eq() {
    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(TrackId::from("t1"));
    set.insert(TrackId::from("t1"));
    set.insert(TrackId::from("t2"));
    assert_eq!(set.len(), 2);
}
