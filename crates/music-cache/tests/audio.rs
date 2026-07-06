//! Integration tests for the on-disk audio cache.
//!
//! Each test uses its own `tempfile::tempdir`; nothing is shared.

use std::time::Duration;

use bytes::Bytes;
use music_cache::{AudioCache, AudioKey, PinOutcome, UnpinOutcome};

fn key(track: &str, bitrate: Option<u32>, codec: &str) -> AudioKey {
    AudioKey {
        track_id: track.to_string(),
        bitrate,
        codec: codec.to_string(),
    }
}

async fn open_cache(regular: u64, pinned: u64) -> (tempfile::TempDir, AudioCache) {
    let dir = tempfile::tempdir().unwrap();
    let cache = AudioCache::open(dir.path(), regular, pinned).await.unwrap();
    (dir, cache)
}

// ---------- AudioKey ----------

#[test]
fn key_canonical_form_is_stable_and_distinguishes_bitrate() {
    let a = key("tr-1", Some(192), "mp3").canonical();
    let b = key("tr-1", Some(320), "mp3").canonical();
    let c = key("tr-1", None, "mp3").canonical();
    assert_ne!(a, b, "different bitrate must produce different keys");
    assert_ne!(a, c, "Some(192) must differ from None");
    assert_eq!(a, key("tr-1", Some(192), "mp3").canonical(), "stable");
}

#[test]
fn key_canonical_form_distinguishes_codec() {
    let a = key("tr-1", Some(192), "mp3").canonical();
    let b = key("tr-1", Some(192), "opus").canonical();
    assert_ne!(a, b);
}

// ---------- get/put round-trip ----------

#[tokio::test]
async fn fresh_cache_returns_none_for_unknown_key() {
    let (_dir, cache) = open_cache(10_000_000, 5_000_000).await;
    assert!(
        cache
            .get(&key("nope", None, "mp3"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn put_then_get_round_trips_body() {
    let (_dir, cache) = open_cache(10_000_000, 5_000_000).await;
    let k = key("tr-1", Some(192), "mp3");
    let body = Bytes::from_static(b"AUDIO-DATA-A");
    let written = cache.put(&k, body.clone()).await.unwrap();
    assert_eq!(written.bytes, body.len() as u64);
    assert!(!written.pinned);

    let read = cache.get(&k).await.unwrap().expect("entry exists");
    assert_eq!(read.bytes, body.len() as u64);
    let on_disk = tokio::fs::read(&read.blob_path).await.unwrap();
    assert_eq!(on_disk, body.as_ref());
}

#[tokio::test]
async fn put_writes_blob_under_cache_root() {
    let (dir, cache) = open_cache(10_000_000, 5_000_000).await;
    let k = key("tr-1", Some(192), "mp3");
    let entry = cache.put(&k, Bytes::from_static(b"x")).await.unwrap();
    assert!(
        entry.blob_path.starts_with(dir.path()),
        "blob_path {} must live under cache root {}",
        entry.blob_path.display(),
        dir.path().display()
    );
    assert!(entry.blob_path.exists());
}

#[tokio::test]
async fn put_replaces_existing_blob_for_same_key() {
    let (_dir, cache) = open_cache(10_000_000, 5_000_000).await;
    let k = key("tr-1", Some(192), "mp3");
    let _first = cache.put(&k, Bytes::from_static(b"AAA")).await.unwrap();
    let second = cache.put(&k, Bytes::from_static(b"BBBB")).await.unwrap();
    let on_disk = tokio::fs::read(&second.blob_path).await.unwrap();
    assert_eq!(on_disk, b"BBBB");
    assert_eq!(second.bytes, 4);
}

// ---------- delete ----------

#[tokio::test]
async fn delete_removes_blob_and_metadata() {
    let (_dir, cache) = open_cache(10_000_000, 5_000_000).await;
    let k = key("tr-1", Some(192), "mp3");
    let entry = cache.put(&k, Bytes::from_static(b"x")).await.unwrap();
    let path = entry.blob_path.clone();
    assert!(cache.delete(&k).await.unwrap());
    assert!(cache.get(&k).await.unwrap().is_none());
    assert!(!path.exists(), "blob file must be unlinked");
}

#[tokio::test]
async fn delete_returns_false_for_unknown_key() {
    let (_dir, cache) = open_cache(10_000_000, 5_000_000).await;
    assert!(!cache.delete(&key("nope", None, "mp3")).await.unwrap());
}

#[tokio::test]
async fn delete_succeeds_even_when_blob_already_gone() {
    // Robustness: external `rm` of the blob file shouldn't break the cache.
    let (_dir, cache) = open_cache(10_000_000, 5_000_000).await;
    let k = key("tr-1", Some(192), "mp3");
    let entry = cache.put(&k, Bytes::from_static(b"x")).await.unwrap();
    tokio::fs::remove_file(&entry.blob_path).await.unwrap();
    assert!(cache.delete(&k).await.unwrap(), "DB row still cleaned");
}

// ---------- touch / LRU ordering ----------

#[tokio::test]
async fn touch_updates_last_accessed_at() {
    let (_dir, cache) = open_cache(10_000_000, 5_000_000).await;
    let k = key("tr-1", Some(192), "mp3");
    let written = cache.put(&k, Bytes::from_static(b"x")).await.unwrap();
    let before = written.last_accessed_at;

    tokio::time::sleep(Duration::from_millis(50)).await;
    cache.touch(&k).await.unwrap();
    let read = cache.get(&k).await.unwrap().unwrap();
    assert!(
        read.last_accessed_at > before,
        "touch must advance last_accessed_at: before={before:?} after={:?}",
        read.last_accessed_at
    );
}

// ---------- LRU eviction ----------

#[tokio::test]
async fn auto_evicts_lru_when_put_pushes_over_budget() {
    // Budget = 10 bytes regular. Insert three 4-byte entries; oldest must go.
    let (_dir, cache) = open_cache(10, 1_000_000).await;
    let a = key("a", None, "mp3");
    let b = key("b", None, "mp3");
    let c = key("c", None, "mp3");
    cache.put(&a, Bytes::from_static(b"AAAA")).await.unwrap();
    sleep_to_advance_clock().await;
    cache.put(&b, Bytes::from_static(b"BBBB")).await.unwrap();
    sleep_to_advance_clock().await;
    cache.put(&c, Bytes::from_static(b"CCCC")).await.unwrap();

    // a was first written, never touched → oldest → evicted.
    assert!(cache.get(&a).await.unwrap().is_none(), "a must be evicted");
    assert!(cache.get(&b).await.unwrap().is_some());
    assert!(cache.get(&c).await.unwrap().is_some());
}

#[tokio::test]
async fn touch_protects_an_entry_from_eviction() {
    let (_dir, cache) = open_cache(10, 1_000_000).await;
    let a = key("a", None, "mp3");
    let b = key("b", None, "mp3");
    let c = key("c", None, "mp3");
    cache.put(&a, Bytes::from_static(b"AAAA")).await.unwrap();
    sleep_to_advance_clock().await;
    cache.put(&b, Bytes::from_static(b"BBBB")).await.unwrap();
    sleep_to_advance_clock().await;
    cache.touch(&a).await.unwrap(); // a now newer than b
    sleep_to_advance_clock().await;
    cache.put(&c, Bytes::from_static(b"CCCC")).await.unwrap();

    // b is now the oldest, should be evicted.
    assert!(cache.get(&a).await.unwrap().is_some(), "a touched recently");
    assert!(cache.get(&b).await.unwrap().is_none(), "b oldest");
    assert!(cache.get(&c).await.unwrap().is_some());
}

#[tokio::test]
async fn lru_eviction_removes_blob_from_disk() {
    let (_dir, cache) = open_cache(10, 1_000_000).await;
    let a = key("a", None, "mp3");
    let b = key("b", None, "mp3");
    let entry_a = cache.put(&a, Bytes::from_static(b"AAAA")).await.unwrap();
    let path_a = entry_a.blob_path.clone();
    sleep_to_advance_clock().await;
    cache
        .put(&b, Bytes::from_static(b"BBBBBBBB"))
        .await
        .unwrap();

    assert!(cache.get(&a).await.unwrap().is_none());
    assert!(!path_a.exists(), "evicted blob file must be unlinked");
}

#[tokio::test]
async fn pinned_entries_are_skipped_by_lru_eviction() {
    // Regular budget = 6 (tight). Pin a, then put b and c (4 bytes each):
    // regular total = 8 > 6 → evict the LRU regular row (b). Pinned a must
    // be skipped even though it's the oldest entry overall.
    let (_dir, cache) = open_cache(6, 1_000).await;
    let a = key("a", None, "mp3");
    let b = key("b", None, "mp3");
    let c = key("c", None, "mp3");
    cache.put(&a, Bytes::from_static(b"AAAA")).await.unwrap();
    assert_eq!(cache.pin(&a).await.unwrap(), PinOutcome::Pinned);
    sleep_to_advance_clock().await;
    cache.put(&b, Bytes::from_static(b"BBBB")).await.unwrap();
    sleep_to_advance_clock().await;
    cache.put(&c, Bytes::from_static(b"CCCC")).await.unwrap();

    // a is pinned (counts against pinned budget, not regular). b is the oldest
    // unpinned entry → evicted.
    assert!(
        cache.get(&a).await.unwrap().is_some(),
        "pinned must survive"
    );
    assert!(
        cache.get(&b).await.unwrap().is_none(),
        "b is oldest unpinned"
    );
    assert!(cache.get(&c).await.unwrap().is_some());
}

// ---------- stats ----------

#[tokio::test]
async fn stats_count_bytes_and_entries_separately_for_pinned() {
    let (_dir, cache) = open_cache(10_000, 10_000).await;
    cache
        .put(&key("a", None, "mp3"), Bytes::from_static(b"AAAA"))
        .await
        .unwrap();
    cache
        .put(&key("b", None, "mp3"), Bytes::from_static(b"BBBBBB"))
        .await
        .unwrap();
    assert_eq!(
        cache.pin(&key("b", None, "mp3")).await.unwrap(),
        PinOutcome::Pinned
    );

    let stats = cache.stats().await.unwrap();
    assert_eq!(stats.regular_count, 1);
    assert_eq!(stats.regular_bytes, 4);
    assert_eq!(stats.pinned_count, 1);
    assert_eq!(stats.pinned_bytes, 6);
    assert_eq!(stats.regular_budget_bytes, 10_000);
    assert_eq!(stats.pinned_budget_bytes, 10_000);
}

// ---------- persistence ----------

#[tokio::test]
async fn cache_survives_close_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let k = key("tr-1", Some(192), "mp3");
    {
        let cache = AudioCache::open(dir.path(), 10_000_000, 5_000_000)
            .await
            .unwrap();
        cache
            .put(&k, Bytes::from_static(b"persist-me"))
            .await
            .unwrap();
    }
    let reopened = AudioCache::open(dir.path(), 10_000_000, 5_000_000)
        .await
        .unwrap();
    let entry = reopened.get(&k).await.unwrap().expect("rehydrated");
    let on_disk = tokio::fs::read(&entry.blob_path).await.unwrap();
    assert_eq!(on_disk, b"persist-me");
}

// ---------- pinning ----------

#[tokio::test]
async fn pin_returns_not_in_cache_when_track_absent() {
    let (_dir, cache) = open_cache(10_000, 10_000).await;
    let outcome = cache.pin(&key("ghost", None, "mp3")).await.unwrap();
    assert_eq!(outcome, PinOutcome::NotInCache);
}

#[tokio::test]
async fn pin_is_idempotent_when_already_pinned() {
    let (_dir, cache) = open_cache(10_000, 10_000).await;
    let k = key("a", None, "mp3");
    cache.put(&k, Bytes::from_static(b"AAAA")).await.unwrap();
    assert_eq!(cache.pin(&k).await.unwrap(), PinOutcome::Pinned);
    assert_eq!(cache.pin(&k).await.unwrap(), PinOutcome::AlreadyPinned);
}

#[tokio::test]
async fn pin_refuses_when_would_exceed_pinned_budget() {
    // Pinned budget = 5. Pinning a 4-byte track is fine; pinning a second
    // 4-byte track would push pinned total to 8 > 5.
    let (_dir, cache) = open_cache(10_000, 5).await;
    let a = key("a", None, "mp3");
    let b = key("b", None, "mp3");
    cache.put(&a, Bytes::from_static(b"AAAA")).await.unwrap();
    cache.put(&b, Bytes::from_static(b"BBBB")).await.unwrap();
    assert_eq!(cache.pin(&a).await.unwrap(), PinOutcome::Pinned);

    let outcome = cache.pin(&b).await.unwrap();
    assert_eq!(outcome, PinOutcome::WouldExceedBudget { over_by: 3 });
    // b must NOT be pinned after refusal.
    assert!(!cache.get(&b).await.unwrap().unwrap().pinned);
}

#[tokio::test]
async fn unpin_returns_not_in_cache_when_track_absent() {
    let (_dir, cache) = open_cache(10_000, 10_000).await;
    let outcome = cache.unpin(&key("ghost", None, "mp3")).await.unwrap();
    assert_eq!(outcome, UnpinOutcome::NotInCache);
}

#[tokio::test]
async fn unpin_returns_not_pinned_when_not_yet_pinned() {
    let (_dir, cache) = open_cache(10_000, 10_000).await;
    let k = key("a", None, "mp3");
    cache.put(&k, Bytes::from_static(b"x")).await.unwrap();
    assert_eq!(cache.unpin(&k).await.unwrap(), UnpinOutcome::NotPinned);
}

#[tokio::test]
async fn unpin_succeeds_then_eviction_can_reach_the_entry() {
    // Regular budget = 5. Pin a (4 bytes), then put b (4 bytes) — both fit
    // because a is in the pinned bucket. Unpin a → regular total = 8 > 5,
    // and a being now-unpinned-but-LRU-touched-on-pin should be evictable.
    let (_dir, cache) = open_cache(5, 1_000).await;
    let a = key("a", None, "mp3");
    let b = key("b", None, "mp3");
    cache.put(&a, Bytes::from_static(b"AAAA")).await.unwrap();
    assert_eq!(cache.pin(&a).await.unwrap(), PinOutcome::Pinned);
    sleep_to_advance_clock().await;
    cache.put(&b, Bytes::from_static(b"BBBB")).await.unwrap();
    sleep_to_advance_clock().await;

    assert_eq!(cache.unpin(&a).await.unwrap(), UnpinOutcome::Unpinned);
    // After unpin: a becomes regular (older) and b is regular (newer).
    // Total regular = 8 > budget 5. Eviction should drop a.
    assert!(
        cache.get(&a).await.unwrap().is_none(),
        "a should be evicted as the LRU regular entry"
    );
    assert!(cache.get(&b).await.unwrap().is_some());
}

#[tokio::test]
async fn list_pinned_returns_only_pinned_entries() {
    let (_dir, cache) = open_cache(10_000, 10_000).await;
    let a = key("a", None, "mp3");
    let b = key("b", None, "mp3");
    let c = key("c", None, "mp3");
    cache.put(&a, Bytes::from_static(b"AAAA")).await.unwrap();
    cache.put(&b, Bytes::from_static(b"BBBB")).await.unwrap();
    cache.put(&c, Bytes::from_static(b"CCCC")).await.unwrap();
    cache.pin(&a).await.unwrap();
    cache.pin(&c).await.unwrap();

    let pinned = cache.list_pinned().await.unwrap();
    let track_ids: std::collections::BTreeSet<_> =
        pinned.iter().map(|e| e.key.track_id.clone()).collect();
    assert_eq!(
        track_ids,
        ["a".to_string(), "c".to_string()].into_iter().collect()
    );
    assert!(pinned.iter().all(|e| e.pinned));
}

#[tokio::test]
async fn pin_survives_re_put_of_same_key() {
    // Re-fetching a track that's already pinned must NOT silently unpin it.
    let (_dir, cache) = open_cache(10_000, 10_000).await;
    let k = key("a", None, "mp3");
    cache.put(&k, Bytes::from_static(b"AAAA")).await.unwrap();
    cache.pin(&k).await.unwrap();
    cache
        .put(&k, Bytes::from_static(b"AAAABBBB"))
        .await
        .unwrap();
    let entry = cache.get(&k).await.unwrap().unwrap();
    assert!(entry.pinned, "re-put must preserve pinned flag");
    assert_eq!(entry.bytes, 8);
}

// ---------- find_by_track + set_budgets ----------

#[tokio::test]
async fn find_by_track_prefers_pinned_across_qualities() {
    let (_dir, cache) = open_cache(10_000, 10_000).await;
    // Two qualities of the same track: an unpinned mp3, a pinned opus.
    let mp3 = key("tr", Some(320), "mp3");
    let opus = key("tr", Some(128), "opus");
    cache.put(&mp3, Bytes::from_static(b"MP3")).await.unwrap();
    cache.put(&opus, Bytes::from_static(b"OPUS")).await.unwrap();
    cache.pin(&opus).await.unwrap();

    let found = cache.find_by_track("tr").await.unwrap().unwrap();
    assert!(found.pinned, "the pinned entry wins");
    assert_eq!(found.key.codec, "opus");

    // Unknown track → None.
    assert!(cache.find_by_track("nope").await.unwrap().is_none());
}

#[tokio::test]
async fn set_budgets_lowers_and_eviction_takes_effect() {
    let (_dir, cache) = open_cache(10_000, 10_000).await;
    cache
        .put(&key("a", None, "mp3"), Bytes::from_static(&[0u8; 400]))
        .await
        .unwrap();
    sleep_to_advance_clock().await;
    cache
        .put(&key("b", None, "mp3"), Bytes::from_static(&[0u8; 400]))
        .await
        .unwrap();
    assert_eq!(cache.regular_budget_bytes(), 10_000);

    // Drop the regular budget below the current total and fit to it.
    cache.set_budgets(500, 10_000);
    assert_eq!(cache.regular_budget_bytes(), 500);
    let total = cache.evict_lru_to_fit().await.unwrap();
    assert!(total <= 500, "eviction honours the new budget, got {total}");
}

// ---------- helpers ----------

async fn sleep_to_advance_clock() {
    // last_accessed_at is stored as unix-millis. A small sleep is enough to
    // give consecutive writes a strictly increasing timestamp.
    tokio::time::sleep(Duration::from_millis(20)).await;
}
