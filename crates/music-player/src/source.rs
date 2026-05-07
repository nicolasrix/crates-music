//! Cache-aware audio source resolution.

use std::future::Future;

use bytes::Bytes;
use music_cache::{AudioCache, AudioKey};

#[derive(Debug, thiserror::Error)]
pub enum ResolveError<E> {
    #[error("cache: {0}")]
    Cache(#[from] music_cache::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("fetch: {0}")]
    Fetch(E),
}

/// Cache-only read. Returns `Ok(None)` on miss, never invokes a fetcher.
/// Used for offline mode where the caller has opted out of the network.
pub async fn read_cached(
    cache: &AudioCache,
    key: &AudioKey,
) -> Result<Option<Bytes>, music_cache::Error> {
    let Some(entry) = cache.get(key).await? else {
        return Ok(None);
    };
    cache.touch(key).await?;
    let bytes = tokio::fs::read(&entry.blob_path).await?;
    Ok(Some(Bytes::from(bytes)))
}

/// Resolve audio bytes for a track:
///   - cache hit: read blob from disk, bump `last_accessed_at`, return bytes;
///   - cache miss: invoke `fetch`, store the result in the cache, return bytes.
///
/// `fetch` is only invoked on a miss. The fetcher's error type `E` is
/// surfaced unchanged via [`ResolveError::Fetch`].
pub async fn resolve_source<F, Fut, E>(
    cache: &AudioCache,
    key: &AudioKey,
    fetch: F,
) -> Result<Bytes, ResolveError<E>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Bytes, E>>,
{
    if let Some(entry) = cache.get(key).await? {
        cache.touch(key).await?;
        let bytes = tokio::fs::read(&entry.blob_path).await?;
        return Ok(Bytes::from(bytes));
    }
    let bytes = fetch().await.map_err(ResolveError::Fetch)?;
    cache.put(key, bytes.clone()).await?;
    Ok(bytes)
}
