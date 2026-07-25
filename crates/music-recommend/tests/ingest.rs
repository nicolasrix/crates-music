//! Tests for the ingest worker.
//!
//! The worker is fed by a `MockFetcher` that returns canned bytes per
//! track id and a wiremock-driven embedder. The recovery semantics
//! (idempotent on (track_id, model_version), transient → failed +
//! retryable, ANN updated atomically with SQLite) are pinned here.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use music_core::TrackId;
use music_recommend::ann::AnnIndex;
use music_recommend::embedder::{EmbedderClient, EmbedderConfig};
use music_recommend::ingest::{
    AudioFetcher, FetchError, IngestOutcome, IngestWorker, IngestWorkerConfig,
};
use music_recommend::store::EmbeddingStore;
use music_recommend::types::{IngestStatus, ModelVersion};
use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIM: usize = 8;

async fn fresh_store() -> (EmbeddingStore, TempDir) {
    // Use the production `open` so the test inherits WAL + busy_timeout
    // settings — concurrent worker tests would otherwise pass on a
    // permissive in-test config and fail in production.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("rec.sqlite");
    let store = EmbeddingStore::open(&db).await.expect("open store");
    (store, dir)
}

struct MockFetcher {
    fetched: AtomicUsize,
    fail: bool,
}

impl MockFetcher {
    fn new() -> Self {
        Self {
            fetched: AtomicUsize::new(0),
            fail: false,
        }
    }
    fn failing() -> Self {
        Self {
            fetched: AtomicUsize::new(0),
            fail: true,
        }
    }
    fn count(&self) -> usize {
        self.fetched.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl AudioFetcher for MockFetcher {
    async fn fetch_clip(&self, track_id: &TrackId) -> Result<Bytes, FetchError> {
        self.fetched.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(FetchError::Transport("network down".into()));
        }
        Ok(Bytes::from(format!(
            "audio-bytes-for-{}",
            track_id.as_str()
        )))
    }
}

fn vector_response(dim: usize) -> serde_json::Value {
    let v: Vec<f32> = (0..dim)
        .map(|i| f32::from(u16::try_from(i).expect("dim fits in u16")) * 0.01)
        .collect();
    json!({
        "vector": v,
        "dim": dim,
        "model_version": "stub-v1"
    })
}

fn build_embedder_for(server: &MockServer) -> EmbedderClient {
    EmbedderClient::new(EmbedderConfig {
        url: server.uri().parse().unwrap(),
        timeout: Duration::from_secs(2),
        bearer_token: None,
    })
    .unwrap()
}

#[tokio::test]
async fn process_one_pending_track_succeeds() {
    let (store, _dir) = fresh_store().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vector_response(DIM)))
        .mount(&server)
        .await;

    store
        .enqueue(&music_recommend::EmbeddingKey::new(
            TrackId::from("t1"),
            ModelVersion::from("stub-v1"),
        ))
        .await
        .unwrap();

    let ann = Arc::new(AnnIndex::open_in_memory(DIM, 16).unwrap());
    let fetcher = Arc::new(MockFetcher::new());
    let embedder = build_embedder_for(&server);
    let worker = IngestWorker::new(IngestWorkerConfig {
        store: store.clone(),
        ann: ann.clone(),
        embedder,
        fetcher: fetcher.clone(),
        model_version: ModelVersion::from("stub-v1"),
        metadata: None,
    });

    let outcome = worker.process_next().await.expect("worker step");
    assert_eq!(outcome, IngestOutcome::Embedded);
    assert_eq!(fetcher.count(), 1);

    // SQLite + ANN both updated.
    let key =
        music_recommend::EmbeddingKey::new(TrackId::from("t1"), ModelVersion::from("stub-v1"));
    assert_eq!(store.status(&key).await.unwrap(), Some(IngestStatus::Done));
    assert_eq!(ann.len().unwrap(), 1);
}

#[tokio::test]
async fn process_next_returns_idle_when_queue_empty() {
    let (store, _dir) = fresh_store().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vector_response(DIM)))
        .mount(&server)
        .await;

    let ann = Arc::new(AnnIndex::open_in_memory(DIM, 16).unwrap());
    let fetcher = Arc::new(MockFetcher::new());
    let embedder = build_embedder_for(&server);
    let worker = IngestWorker::new(IngestWorkerConfig {
        store,
        ann,
        embedder,
        fetcher: fetcher.clone(),
        model_version: ModelVersion::from("stub-v1"),
        metadata: None,
    });

    let outcome = worker.process_next().await.expect("worker step");
    assert_eq!(outcome, IngestOutcome::Idle);
    assert_eq!(fetcher.count(), 0);
}

#[tokio::test]
async fn fetch_failure_marks_failed_and_continues() {
    let (store, _dir) = fresh_store().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vector_response(DIM)))
        .mount(&server)
        .await;

    let key =
        music_recommend::EmbeddingKey::new(TrackId::from("t1"), ModelVersion::from("stub-v1"));
    store.enqueue(&key).await.unwrap();

    let ann = Arc::new(AnnIndex::open_in_memory(DIM, 16).unwrap());
    let fetcher = Arc::new(MockFetcher::failing());
    let embedder = build_embedder_for(&server);
    let worker = IngestWorker::new(IngestWorkerConfig {
        store: store.clone(),
        ann: ann.clone(),
        embedder,
        fetcher: fetcher.clone(),
        model_version: ModelVersion::from("stub-v1"),
        metadata: None,
    });

    let outcome = worker.process_next().await.expect("worker step");
    assert_eq!(outcome, IngestOutcome::Failed);
    assert_eq!(
        store.status(&key).await.unwrap(),
        Some(IngestStatus::Failed)
    );
    assert_eq!(
        ann.len().unwrap(),
        0,
        "ANN must not be touched when fetch fails"
    );
}

#[tokio::test]
async fn embedder_503_marks_failed_and_is_retryable() {
    // Sidecar returns 503 (not loaded). The row is marked failed but
    // reset_failed → not_started lets a retry succeed once the
    // sidecar is back. This is the "ingest while embedder warmed up"
    // recovery path.
    let (store, _dir) = fresh_store().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(
            ResponseTemplate::new(503).set_body_json(json!({ "detail": "model not loaded" })),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vector_response(DIM)))
        .mount(&server)
        .await;

    let key =
        music_recommend::EmbeddingKey::new(TrackId::from("t1"), ModelVersion::from("stub-v1"));
    store.enqueue(&key).await.unwrap();

    let ann = Arc::new(AnnIndex::open_in_memory(DIM, 16).unwrap());
    let fetcher = Arc::new(MockFetcher::new());
    let embedder = build_embedder_for(&server);
    let worker = IngestWorker::new(IngestWorkerConfig {
        store: store.clone(),
        ann: ann.clone(),
        embedder,
        fetcher: fetcher.clone(),
        model_version: ModelVersion::from("stub-v1"),
        metadata: None,
    });

    // First step: embedder returns 503 → row marked failed.
    let outcome = worker.process_next().await.unwrap();
    assert_eq!(outcome, IngestOutcome::Failed);

    // Reset and retry — second step finds the row in not_started and
    // succeeds against the now-200 mock.
    let reset = store
        .reset_failed(&ModelVersion::from("stub-v1"))
        .await
        .unwrap();
    assert_eq!(reset, 1);
    let outcome = worker.process_next().await.unwrap();
    assert_eq!(outcome, IngestOutcome::Embedded);
    assert_eq!(store.status(&key).await.unwrap(), Some(IngestStatus::Done));
    assert_eq!(ann.len().unwrap(), 1);
}

#[tokio::test]
async fn worker_skips_other_model_versions() {
    let (store, _dir) = fresh_store().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vector_response(DIM)))
        .mount(&server)
        .await;

    // Queue under v2; worker is configured for v1 — must idle.
    store
        .enqueue(&music_recommend::EmbeddingKey::new(
            TrackId::from("t1"),
            ModelVersion::from("stub-v2"),
        ))
        .await
        .unwrap();

    let ann = Arc::new(AnnIndex::open_in_memory(DIM, 16).unwrap());
    let fetcher = Arc::new(MockFetcher::new());
    let embedder = build_embedder_for(&server);
    let worker = IngestWorker::new(IngestWorkerConfig {
        store,
        ann,
        embedder,
        fetcher: fetcher.clone(),
        model_version: ModelVersion::from("stub-v1"),
        metadata: None,
    });

    let outcome = worker.process_next().await.unwrap();
    assert_eq!(outcome, IngestOutcome::Idle);
    assert_eq!(fetcher.count(), 0);
}

#[tokio::test]
async fn drains_until_idle() {
    let (store, _dir) = fresh_store().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vector_response(DIM)))
        .mount(&server)
        .await;

    for i in 0..5 {
        store
            .enqueue(&music_recommend::EmbeddingKey::new(
                TrackId::from(format!("t{i}")),
                ModelVersion::from("stub-v1"),
            ))
            .await
            .unwrap();
    }

    let ann = Arc::new(AnnIndex::open_in_memory(DIM, 16).unwrap());
    let fetcher = Arc::new(MockFetcher::new());
    let embedder = build_embedder_for(&server);
    let worker = IngestWorker::new(IngestWorkerConfig {
        store: store.clone(),
        ann: ann.clone(),
        embedder,
        fetcher,
        model_version: ModelVersion::from("stub-v1"),
        metadata: None,
    });

    let processed = worker.drain().await.expect("drain");
    assert_eq!(processed.embedded, 5);
    assert_eq!(processed.failed, 0);
    assert_eq!(ann.len().unwrap(), 5);

    let counts = store.counts(&ModelVersion::from("stub-v1")).await.unwrap();
    assert_eq!(counts.done, 5);
    assert_eq!(counts.not_started, 0);
}

#[tokio::test]
async fn concurrent_drainers_split_queue_without_double_processing() {
    // Phase-2 concurrency contract: spawn N tasks each calling
    // `worker.drain()` on a shared Arc. SQLite's UPDATE ... RETURNING
    // in claim_next must keep two tasks from claiming the same row.
    // Total embedded must equal the enqueued count exactly — neither
    // less (lost row) nor more (double-processed row).
    const ENQUEUED: usize = 16;
    const WORKERS: usize = 4;

    let (store, _dir) = fresh_store().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vector_response(DIM)))
        .mount(&server)
        .await;

    for i in 0..ENQUEUED {
        store
            .enqueue(&music_recommend::EmbeddingKey::new(
                TrackId::from(format!("t{i:02}")),
                ModelVersion::from("stub-v1"),
            ))
            .await
            .unwrap();
    }

    let ann = Arc::new(AnnIndex::open_in_memory(DIM, 16).unwrap());
    let fetcher = Arc::new(MockFetcher::new());
    let embedder = build_embedder_for(&server);
    let worker = Arc::new(IngestWorker::new(IngestWorkerConfig {
        store: store.clone(),
        ann: ann.clone(),
        embedder,
        fetcher,
        model_version: ModelVersion::from("stub-v1"),
        metadata: None,
    }));

    let mut handles = Vec::with_capacity(WORKERS);
    for _ in 0..WORKERS {
        let w = Arc::clone(&worker);
        handles.push(tokio::spawn(async move { w.drain().await }));
    }

    let mut total_embedded = 0u64;
    let mut total_failed = 0u64;
    for h in handles {
        let stats = h.await.expect("task join").expect("drain ok");
        total_embedded += stats.embedded;
        total_failed += stats.failed;
    }

    assert_eq!(total_embedded, ENQUEUED as u64, "every row processed once");
    assert_eq!(total_failed, 0);

    let counts = store.counts(&ModelVersion::from("stub-v1")).await.unwrap();
    assert_eq!(counts.done, ENQUEUED as u64);
    assert_eq!(counts.not_started, 0);
    assert_eq!(counts.in_progress, 0);

    // ANN length matches: no duplicate upserts and no missing tracks.
    assert_eq!(ann.len().unwrap(), ENQUEUED);
}

#[tokio::test]
async fn rebuild_ann_from_store_repopulates_index() {
    // The crash-recovery / fresh-startup path: the ANN file is
    // missing, but SQLite has 5 done embeddings. `rebuild_ann` walks
    // the store and re-feeds everything.
    let (store, _dir) = fresh_store().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vector_response(DIM)))
        .mount(&server)
        .await;

    let ann_pre = AnnIndex::open_in_memory(DIM, 16).unwrap();
    let fetcher = Arc::new(MockFetcher::new());
    let embedder = build_embedder_for(&server);
    let pre_worker = IngestWorker::new(IngestWorkerConfig {
        store: store.clone(),
        ann: Arc::new(ann_pre),
        embedder,
        fetcher: fetcher.clone(),
        model_version: ModelVersion::from("stub-v1"),
        metadata: None,
    });

    for i in 0..3 {
        store
            .enqueue(&music_recommend::EmbeddingKey::new(
                TrackId::from(format!("t{i}")),
                ModelVersion::from("stub-v1"),
            ))
            .await
            .unwrap();
    }
    pre_worker.drain().await.unwrap();

    // Simulate the ANN file being deleted: open a fresh empty index
    // and call rebuild_ann_from_store.
    let fresh_ann = Arc::new(AnnIndex::open_in_memory(DIM, 16).unwrap());
    assert_eq!(fresh_ann.len().unwrap(), 0);

    music_recommend::ingest::rebuild_ann_from_store(
        &store,
        &fresh_ann,
        &ModelVersion::from("stub-v1"),
    )
    .await
    .expect("rebuild");

    assert_eq!(fresh_ann.len().unwrap(), 3);
}
