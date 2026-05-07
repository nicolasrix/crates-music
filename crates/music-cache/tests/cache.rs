//! Integration tests for the SQLite-backed catalog cache.
//!
//! Each test gets its own temporary file (or in-memory DB) — no shared state.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use music_cache::{Cache, etag_for};

#[tokio::test]
async fn fresh_cache_returns_none_for_unknown_key() {
    let cache = Cache::open_in_memory().await.unwrap();
    assert!(cache.get("nope").await.unwrap().is_none());
}

#[tokio::test]
async fn put_then_get_round_trips_body_and_etag() {
    let cache = Cache::open_in_memory().await.unwrap();
    let body = Bytes::from_static(b"{\"hello\":\"world\"}");
    let written = cache
        .put("k1", body.clone(), Duration::from_secs(45))
        .await
        .unwrap();
    let read = cache.get("k1").await.unwrap().expect("entry exists");
    assert_eq!(read.body, body);
    assert_eq!(read.etag, written.etag);
    assert_eq!(read.etag, etag_for(&body));
}

#[tokio::test]
async fn etag_is_stable_across_identical_writes() {
    let cache = Cache::open_in_memory().await.unwrap();
    let body = Bytes::from_static(b"same body");
    let a = cache
        .put("k", body.clone(), Duration::from_secs(45))
        .await
        .unwrap();
    let b = cache.put("k", body, Duration::from_secs(45)).await.unwrap();
    assert_eq!(a.etag, b.etag, "ETag must be content-addressed");
}

#[tokio::test]
async fn etag_differs_across_different_bodies() {
    let cache = Cache::open_in_memory().await.unwrap();
    let a = cache
        .put("a", Bytes::from_static(b"alpha"), Duration::from_secs(45))
        .await
        .unwrap();
    let b = cache
        .put("b", Bytes::from_static(b"beta"), Duration::from_secs(45))
        .await
        .unwrap();
    assert_ne!(a.etag, b.etag);
}

#[tokio::test]
async fn etag_for_returns_16_hex_chars() {
    let etag = etag_for(b"anything");
    assert_eq!(etag.len(), 16);
    assert!(etag.chars().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn entry_is_fresh_within_ttl_and_stale_after() {
    let cache = Cache::open_in_memory().await.unwrap();
    let body = Bytes::from_static(b"x");
    let entry = cache.put("k", body, Duration::from_secs(30)).await.unwrap();
    let written_at = entry.fetched_at;
    assert!(entry.is_fresh(written_at));
    assert!(entry.is_fresh(written_at + Duration::from_secs(29)));
    assert!(!entry.is_fresh(written_at + Duration::from_secs(31)));
}

#[tokio::test]
async fn delete_removes_an_entry() {
    let cache = Cache::open_in_memory().await.unwrap();
    cache
        .put("k", Bytes::from_static(b"v"), Duration::from_secs(45))
        .await
        .unwrap();
    assert!(cache.delete("k").await.unwrap());
    assert!(cache.get("k").await.unwrap().is_none());
}

#[tokio::test]
async fn delete_returns_false_for_unknown_key() {
    let cache = Cache::open_in_memory().await.unwrap();
    assert!(!cache.delete("nope").await.unwrap());
}

#[tokio::test]
async fn expire_before_removes_only_stale_entries() {
    let cache = Cache::open_in_memory().await.unwrap();
    let now = SystemTime::now();
    let stale_age = now - Duration::from_secs(3601);
    cache
        .insert_for_test(
            "stale",
            Bytes::from_static(b"old"),
            stale_age,
            Duration::from_secs(45),
        )
        .await
        .unwrap();
    cache
        .put("fresh", Bytes::from_static(b"new"), Duration::from_secs(45))
        .await
        .unwrap();
    let removed = cache.expire_before(now).await.unwrap();
    assert_eq!(removed, 1);
    assert!(cache.get("stale").await.unwrap().is_none());
    assert!(cache.get("fresh").await.unwrap().is_some());
}

#[tokio::test]
async fn cache_survives_close_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.sqlite");

    {
        let cache = Cache::open(&path).await.unwrap();
        cache
            .put(
                "persistent",
                Bytes::from_static(b"value"),
                Duration::from_secs(45),
            )
            .await
            .unwrap();
    }

    let reopened = Cache::open(&path).await.unwrap();
    let entry = reopened.get("persistent").await.unwrap().unwrap();
    assert_eq!(entry.body, Bytes::from_static(b"value"));
}

#[tokio::test]
async fn fetched_at_uses_unix_epoch_seconds_resolution() {
    let cache = Cache::open_in_memory().await.unwrap();
    let entry = cache
        .put("k", Bytes::from_static(b"v"), Duration::from_secs(45))
        .await
        .unwrap();
    let secs = entry
        .fetched_at
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(now.abs_diff(secs) <= 5, "fetched_at should be near now");
}
