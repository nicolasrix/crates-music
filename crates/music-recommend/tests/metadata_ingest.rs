//! Tests for the ingest worker's metadata-population side-effect.
//!
//! The hook is best-effort: it runs *before* audio fetch, swallows
//! its own failures, and never blocks the embedding pipeline.
//! Specifically:
//!
//!   - happy path → metadata row written, embedding still happens
//!   - metadata fetch errors → logged, audio + embed still proceed
//!   - audio fetch fails → metadata row still ends up persisted (we
//!     fetched it before audio); embedding marked failed
//!   - re-ingest of same track → metadata UPSERT, no duplicates

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use music_core::TrackId;
use music_recommend::ann::AnnIndex;
use music_recommend::embedder::{EmbedderClient, EmbedderConfig};
use music_recommend::ingest::{
    AudioFetcher, FetchError, IngestOutcome, IngestWorker, IngestWorkerConfig,
    MetadataFetcher, MetadataIngest,
};
use music_recommend::metadata::{
    MetadataStore, TrackMetadata, backfill_metadata, normalize_title,
};
use music_recommend::store::EmbeddingStore;
use music_recommend::types::ModelVersion;
use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIM: usize = 8;

async fn fresh_stores() -> (EmbeddingStore, MetadataStore, TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("rec.sqlite");
    let store = EmbeddingStore::open(&db).await.expect("open store");
    // Both stores share the same SQLite pool — that's the production
    // shape (one DB file, two stores).
    let metadata = MetadataStore::new(store.pool().clone());
    (store, metadata, dir)
}

struct OkAudioFetcher;

#[async_trait::async_trait]
impl AudioFetcher for OkAudioFetcher {
    async fn fetch_clip(&self, track_id: &TrackId) -> Result<Bytes, FetchError> {
        Ok(Bytes::from(format!("audio-{}", track_id.as_str())))
    }
}

struct FailingAudioFetcher;

#[async_trait::async_trait]
impl AudioFetcher for FailingAudioFetcher {
    async fn fetch_clip(&self, _track_id: &TrackId) -> Result<Bytes, FetchError> {
        Err(FetchError::Transport("audio down".into()))
    }
}

struct CannedMetadataFetcher {
    calls: AtomicUsize,
}

impl CannedMetadataFetcher {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl MetadataFetcher for CannedMetadataFetcher {
    async fn fetch_metadata(&self, track_id: &TrackId) -> Result<TrackMetadata, FetchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let title = format!("Title for {}", track_id.as_str());
        Ok(TrackMetadata {
            track_id: track_id.clone(),
            artist_id: Some("ar1".into()),
            artist: "Some Artist".into(),
            album_id: Some("al1".into()),
            album: Some("Some Album".into()),
            title_normalized: normalize_title(&title),
            title,
            duration_seconds: Some(200),
            genre: None,
            year: Some(2020),
            track_number: Some(3),
            disc_number: Some(1),
            bpm: None,
            musical_key: None,
        })
    }
}

struct FailingMetadataFetcher;

#[async_trait::async_trait]
impl MetadataFetcher for FailingMetadataFetcher {
    async fn fetch_metadata(&self, _track_id: &TrackId) -> Result<TrackMetadata, FetchError> {
        Err(FetchError::Transport("getSong down".into()))
    }
}

fn vector_response(dim: usize) -> serde_json::Value {
    let v: Vec<f32> = (0..dim)
        .map(|i| f32::from(u16::try_from(i).expect("dim fits in u16")) * 0.01)
        .collect();
    json!({
        "vector": v,
        "dim": dim,
        "model_version": "stub-v1",
    })
}

fn build_embedder(server: &MockServer) -> EmbedderClient {
    EmbedderClient::new(EmbedderConfig {
        url: server.uri().parse().unwrap(),
        timeout: Duration::from_secs(2),
    })
    .unwrap()
}

async fn enqueue(store: &EmbeddingStore, id: &str) {
    store
        .enqueue(&music_recommend::EmbeddingKey::new(
            TrackId::from(id),
            ModelVersion::from("stub-v1"),
        ))
        .await
        .unwrap();
}

#[tokio::test]
async fn happy_path_populates_metadata_and_embeds() {
    let (store, metadata, _dir) = fresh_stores().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vector_response(DIM)))
        .mount(&server)
        .await;

    enqueue(&store, "t1").await;

    let ann = Arc::new(AnnIndex::open_in_memory(DIM, 16).unwrap());
    let metadata_fetcher = Arc::new(CannedMetadataFetcher::new());
    let worker = IngestWorker::new(IngestWorkerConfig {
        store: store.clone(),
        ann: ann.clone(),
        embedder: build_embedder(&server),
        fetcher: Arc::new(OkAudioFetcher),
        model_version: ModelVersion::from("stub-v1"),
        metadata: Some(MetadataIngest {
            store: metadata.clone(),
            fetcher: metadata_fetcher.clone(),
        }),
    });

    let outcome = worker.process_next().await.unwrap();
    assert_eq!(outcome, IngestOutcome::Embedded);

    // Metadata fetcher was called exactly once.
    assert_eq!(metadata_fetcher.calls(), 1);

    // Metadata row is persisted.
    let m = metadata
        .get(&TrackId::from("t1"))
        .await
        .unwrap()
        .expect("metadata row should exist");
    assert_eq!(m.artist, "Some Artist");
    assert_eq!(m.title, "Title for t1");
    assert_eq!(m.title_normalized, "title for t1");
}

#[tokio::test]
async fn metadata_disabled_skips_population() {
    // metadata: None — old behaviour, no metadata table touched.
    let (store, metadata, _dir) = fresh_stores().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vector_response(DIM)))
        .mount(&server)
        .await;

    enqueue(&store, "t1").await;

    let ann = Arc::new(AnnIndex::open_in_memory(DIM, 16).unwrap());
    let worker = IngestWorker::new(IngestWorkerConfig {
        store: store.clone(),
        ann,
        embedder: build_embedder(&server),
        fetcher: Arc::new(OkAudioFetcher),
        model_version: ModelVersion::from("stub-v1"),
        metadata: None,
    });

    let outcome = worker.process_next().await.unwrap();
    assert_eq!(outcome, IngestOutcome::Embedded);
    assert_eq!(metadata.count().await.unwrap(), 0);
}

#[tokio::test]
async fn metadata_fetch_failure_does_not_tank_ingest() {
    // Metadata fetcher errors → logged, embedding still succeeds.
    let (store, metadata, _dir) = fresh_stores().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vector_response(DIM)))
        .mount(&server)
        .await;

    enqueue(&store, "t1").await;

    let ann = Arc::new(AnnIndex::open_in_memory(DIM, 16).unwrap());
    let worker = IngestWorker::new(IngestWorkerConfig {
        store: store.clone(),
        ann,
        embedder: build_embedder(&server),
        fetcher: Arc::new(OkAudioFetcher),
        model_version: ModelVersion::from("stub-v1"),
        metadata: Some(MetadataIngest {
            store: metadata.clone(),
            fetcher: Arc::new(FailingMetadataFetcher),
        }),
    });

    let outcome = worker.process_next().await.unwrap();
    assert_eq!(outcome, IngestOutcome::Embedded);
    // No metadata row was written.
    assert_eq!(metadata.count().await.unwrap(), 0);
}

#[tokio::test]
async fn metadata_persisted_even_when_audio_fetch_fails() {
    // The hook runs BEFORE audio fetch, so a downstream audio failure
    // still leaves a metadata row behind. This is intentional: the
    // metadata cache is partly there to feed the recommend handlers in
    // degraded mode, and we want it as warm as possible regardless of
    // whether embedding succeeded.
    let (store, metadata, _dir) = fresh_stores().await;
    let server = MockServer::start().await;
    // Embedder mock not required — audio fetch fails first.
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    enqueue(&store, "t1").await;

    let ann = Arc::new(AnnIndex::open_in_memory(DIM, 16).unwrap());
    let metadata_fetcher = Arc::new(CannedMetadataFetcher::new());
    let worker = IngestWorker::new(IngestWorkerConfig {
        store: store.clone(),
        ann,
        embedder: build_embedder(&server),
        fetcher: Arc::new(FailingAudioFetcher),
        model_version: ModelVersion::from("stub-v1"),
        metadata: Some(MetadataIngest {
            store: metadata.clone(),
            fetcher: metadata_fetcher.clone(),
        }),
    });

    let outcome = worker.process_next().await.unwrap();
    assert_eq!(outcome, IngestOutcome::Failed);
    assert_eq!(metadata_fetcher.calls(), 1);
    // Metadata row IS present despite audio failure.
    let m = metadata.get(&TrackId::from("t1")).await.unwrap();
    assert!(m.is_some(), "metadata row should be written before audio fetch");
}

#[tokio::test]
async fn metadata_upsert_is_idempotent_across_re_ingest() {
    // Embed track twice (after reset). Metadata fetcher returns same
    // payload both times → upsert collapses to one row, count stays 1.
    let (store, metadata, _dir) = fresh_stores().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vector_response(DIM)))
        .mount(&server)
        .await;

    enqueue(&store, "t1").await;

    let ann = Arc::new(AnnIndex::open_in_memory(DIM, 16).unwrap());
    let metadata_fetcher = Arc::new(CannedMetadataFetcher::new());
    let worker = IngestWorker::new(IngestWorkerConfig {
        store: store.clone(),
        ann: ann.clone(),
        embedder: build_embedder(&server),
        fetcher: Arc::new(OkAudioFetcher),
        model_version: ModelVersion::from("stub-v1"),
        metadata: Some(MetadataIngest {
            store: metadata.clone(),
            fetcher: metadata_fetcher.clone(),
        }),
    });

    worker.process_next().await.unwrap();
    assert_eq!(metadata.count().await.unwrap(), 1);

    // Re-enqueue (no-op for done rows; we forcibly reset to simulate
    // re-ingest). reset_failed only operates on failed rows, so we
    // re-create a fresh enqueue under the same id to drive a 2nd pass.
    // The embedding store uses INSERT OR IGNORE so this is a no-op,
    // which is exactly the "track already done, queue won't re-process"
    // semantic. To force a real re-ingest, mark_failed + reset_failed.
    store
        .mark_failed(
            &music_recommend::EmbeddingKey::new(
                TrackId::from("t1"),
                ModelVersion::from("stub-v1"),
            ),
            "test forced",
        )
        .await
        .unwrap();
    store
        .reset_failed(&ModelVersion::from("stub-v1"))
        .await
        .unwrap();

    worker.process_next().await.unwrap();
    assert_eq!(metadata_fetcher.calls(), 2);
    // Still one row — UPSERT collapsed both writes.
    assert_eq!(metadata.count().await.unwrap(), 1);
}

// --- backfill_metadata ---

/// Insert N pre-embedded rows into the SQLite store. The shape mirrors
/// what the worker would produce: status='done' under model_version.
async fn seed_done_embeddings(store: &EmbeddingStore, ids: &[&str]) {
    for id in ids {
        let key = music_recommend::EmbeddingKey::new(
            TrackId::from(*id),
            ModelVersion::from("stub-v1"),
        );
        store.enqueue(&key).await.unwrap();
        // Bypass the worker — we just want the row to look done.
        let dim = 4;
        let v = vec![0.1f32; dim];
        store
            .mark_done(&music_recommend::Embedding {
                key,
                vector: v,
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn backfill_populates_missing_rows_and_skips_present_ones() {
    let (store, metadata, _dir) = fresh_stores().await;
    seed_done_embeddings(&store, &["t1", "t2", "t3"]).await;

    // t2 already has metadata — backfill should leave it alone.
    metadata
        .upsert(&TrackMetadata {
            track_id: TrackId::from("t2"),
            artist_id: None,
            artist: "preexisting".into(),
            album_id: None,
            album: None,
            title: "preexisting".into(),
            title_normalized: normalize_title("preexisting"),
            duration_seconds: None,
            genre: None,
            year: None,
            track_number: None,
            disc_number: None,
            bpm: None,
            musical_key: None,
        })
        .await
        .unwrap();

    let fetcher = CannedMetadataFetcher::new();
    let stats = backfill_metadata(&metadata, &fetcher, &ModelVersion::from("stub-v1"))
        .await
        .unwrap();
    assert_eq!(stats.upserted, 2); // t1 + t3
    assert_eq!(stats.fetch_failed, 0);
    assert_eq!(fetcher.calls(), 2);

    // t2 keeps its preexisting "preexisting" artist — backfill didn't touch it.
    let t2 = metadata.get(&TrackId::from("t2")).await.unwrap().unwrap();
    assert_eq!(t2.artist, "preexisting");
    // t1 + t3 have the canned-fetcher artist now.
    let t1 = metadata.get(&TrackId::from("t1")).await.unwrap().unwrap();
    assert_eq!(t1.artist, "Some Artist");
}

#[tokio::test]
async fn backfill_is_idempotent() {
    let (store, metadata, _dir) = fresh_stores().await;
    seed_done_embeddings(&store, &["t1", "t2"]).await;
    let fetcher = CannedMetadataFetcher::new();

    let s1 = backfill_metadata(&metadata, &fetcher, &ModelVersion::from("stub-v1"))
        .await
        .unwrap();
    assert_eq!(s1.upserted, 2);

    // Second pass — every row already cached, nothing fetched.
    let s2 = backfill_metadata(&metadata, &fetcher, &ModelVersion::from("stub-v1"))
        .await
        .unwrap();
    assert_eq!(s2.upserted, 0);
    assert_eq!(s2.fetch_failed, 0);
    // Fetcher was called exactly twice across both runs (the first
    // pass) — the second pass found no missing rows.
    assert_eq!(fetcher.calls(), 2);
}

#[tokio::test]
async fn backfill_with_no_missing_rows_is_a_noop() {
    let (_store, metadata, _dir) = fresh_stores().await;
    let fetcher = CannedMetadataFetcher::new();
    let stats = backfill_metadata(&metadata, &fetcher, &ModelVersion::from("stub-v1"))
        .await
        .unwrap();
    assert_eq!(stats.upserted, 0);
    assert_eq!(stats.fetch_failed, 0);
    assert_eq!(fetcher.calls(), 0);
}

#[tokio::test]
async fn backfill_skips_rows_whose_fetch_fails_without_looping() {
    // Anti-livelock: if the fetcher returns errors for every row, the
    // batch makes zero progress — the loop must bail rather than walk
    // the same rows forever.
    let (store, metadata, _dir) = fresh_stores().await;
    seed_done_embeddings(&store, &["t1", "t2"]).await;

    let stats = backfill_metadata(
        &metadata,
        &FailingMetadataFetcher,
        &ModelVersion::from("stub-v1"),
    )
    .await
    .unwrap();
    assert_eq!(stats.upserted, 0);
    assert_eq!(stats.fetch_failed, 2);
    // Metadata table is still empty.
    assert_eq!(metadata.count().await.unwrap(), 0);
}

#[tokio::test]
async fn backfill_only_walks_rows_for_the_given_model_version() {
    let (store, metadata, _dir) = fresh_stores().await;
    // One row under stub-v1, one row under a different version.
    seed_done_embeddings(&store, &["t1"]).await;
    let other_key = music_recommend::EmbeddingKey::new(
        TrackId::from("t2"),
        ModelVersion::from("other-v1"),
    );
    store.enqueue(&other_key).await.unwrap();
    store
        .mark_done(&music_recommend::Embedding {
            key: other_key,
            vector: vec![0.1f32; 4],
        })
        .await
        .unwrap();

    let fetcher = CannedMetadataFetcher::new();
    let stats = backfill_metadata(&metadata, &fetcher, &ModelVersion::from("stub-v1"))
        .await
        .unwrap();
    // t1 only — t2 isn't in scope for stub-v1 backfill.
    assert_eq!(stats.upserted, 1);
    assert_eq!(fetcher.calls(), 1);
    let t2_present = metadata.get(&TrackId::from("t2")).await.unwrap();
    assert!(t2_present.is_none());
}
