//! `read_cached`: cache-only lookup. Used for offline mode where the network
//! is known unavailable or the user has explicitly opted out of it.

use bytes::Bytes;
use music_cache::{AudioCache, AudioKey};
use music_player::read_cached;

fn key() -> AudioKey {
    AudioKey {
        track_id: "tr-1".to_string(),
        bitrate: Some(192),
        codec: "mp3".to_string(),
    }
}

async fn cache() -> (tempfile::TempDir, AudioCache) {
    let dir = tempfile::tempdir().unwrap();
    let cache = AudioCache::open(dir.path(), 10_000_000, 5_000_000)
        .await
        .unwrap();
    (dir, cache)
}

#[tokio::test]
async fn read_cached_returns_some_with_bytes_on_hit() {
    let (_dir, cache) = cache().await;
    let k = key();
    cache
        .put(&k, Bytes::from_static(b"OFFLINE-PLAYABLE"))
        .await
        .unwrap();

    let bytes = read_cached(&cache, &k).await.unwrap();
    assert_eq!(bytes.as_deref(), Some(&b"OFFLINE-PLAYABLE"[..]));
}

#[tokio::test]
async fn read_cached_returns_none_on_miss() {
    let (_dir, cache) = cache().await;
    let bytes = read_cached(&cache, &key()).await.unwrap();
    assert!(bytes.is_none());
}

#[tokio::test]
async fn read_cached_touches_entry_on_hit() {
    use std::time::Duration;
    let (_dir, cache) = cache().await;
    let k = key();
    let entry = cache.put(&k, Bytes::from_static(b"X")).await.unwrap();
    let before = entry.last_accessed_at;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let _ = read_cached(&cache, &k).await.unwrap();

    let after = cache.get(&k).await.unwrap().unwrap().last_accessed_at;
    assert!(after > before, "read_cached must advance LRU timestamp");
}
