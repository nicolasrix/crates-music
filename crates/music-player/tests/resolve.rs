//! `resolve_source` tests: cache-or-fetch plumbing for audio playback.
//!
//! Rodio playback itself is not tested — it requires an audio device. The
//! interesting logic (cache hit short-circuits the fetcher, miss caches the
//! body, errors propagate) lives in `resolve_source` and is fully covered.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use music_cache::{AudioCache, AudioKey};
use music_player::{ResolveError, resolve_source};

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

#[derive(Debug, thiserror::Error)]
#[error("synthetic fetch failure")]
struct FetchFailure;

#[tokio::test]
async fn resolve_returns_cached_bytes_without_calling_fetcher() {
    let (_dir, cache) = cache().await;
    let k = key();
    cache
        .put(&k, Bytes::from_static(b"AUDIO-CACHED"))
        .await
        .unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_handle = calls.clone();
    let result = resolve_source::<_, _, FetchFailure>(&cache, &k, || async move {
        calls_handle.fetch_add(1, Ordering::SeqCst);
        Err(FetchFailure)
    })
    .await
    .unwrap();

    assert_eq!(result.as_ref(), b"AUDIO-CACHED");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "fetcher must not be called on cache hit"
    );
}

#[tokio::test]
async fn resolve_calls_fetcher_on_miss_and_caches_result() {
    let (_dir, cache) = cache().await;
    let k = key();

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_handle = calls.clone();
    let result = resolve_source::<_, _, FetchFailure>(&cache, &k, || async move {
        calls_handle.fetch_add(1, Ordering::SeqCst);
        Ok(Bytes::from_static(b"AUDIO-FRESH"))
    })
    .await
    .unwrap();

    assert_eq!(result.as_ref(), b"AUDIO-FRESH");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Second call: should now be a cache hit (no fetcher invocation).
    let second_calls = Arc::new(AtomicUsize::new(0));
    let second_handle = second_calls.clone();
    let _ = resolve_source::<_, _, FetchFailure>(&cache, &k, || async move {
        second_handle.fetch_add(1, Ordering::SeqCst);
        Err(FetchFailure)
    })
    .await
    .unwrap();
    assert_eq!(second_calls.load(Ordering::SeqCst), 0);

    // And the cache really has it on disk.
    let entry = cache.get(&k).await.unwrap().unwrap();
    let on_disk = tokio::fs::read(&entry.blob_path).await.unwrap();
    assert_eq!(on_disk, b"AUDIO-FRESH");
}

#[tokio::test]
async fn resolve_propagates_fetcher_error() {
    let (_dir, cache) = cache().await;
    let k = key();

    let err = resolve_source::<_, _, FetchFailure>(&cache, &k, || async { Err(FetchFailure) })
        .await
        .unwrap_err();
    assert!(
        matches!(err, ResolveError::Fetch(_)),
        "expected ResolveError::Fetch, got {err:?}"
    );

    // And nothing was cached.
    assert!(cache.get(&k).await.unwrap().is_none());
}

#[tokio::test]
async fn resolve_touches_cache_on_hit() {
    let (_dir, cache) = cache().await;
    let k = key();
    let entry = cache.put(&k, Bytes::from_static(b"AUDIO")).await.unwrap();
    let before = entry.last_accessed_at;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let _ = resolve_source::<_, _, FetchFailure>(&cache, &k, || async { Err(FetchFailure) })
        .await
        .unwrap();

    let after = cache.get(&k).await.unwrap().unwrap().last_accessed_at;
    assert!(
        after > before,
        "resolve must touch the entry on hit; before={before:?} after={after:?}"
    );
}
