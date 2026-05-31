//! Integration tests for the embedding store.
//!
//! Each test gets its own SQLite file (tempdir) so they're fully
//! isolated. We run the store's migrations as part of setup so the
//! tests double as a sanity check on the SQL.

use music_core::TrackId;
use music_recommend::store::{EmbeddingStore, MIGRATIONS};
use music_recommend::types::{Embedding, EmbeddingKey, IngestStatus, ModelVersion};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tempfile::TempDir;

async fn fresh_store() -> (EmbeddingStore, TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("rec.sqlite");
    // `.filename()` is unambiguous about absolute vs relative paths;
    // the `sqlite://` URL form swallows the leading `/` on Linux and
    // silently opens a different DB per connection.
    let opts = SqliteConnectOptions::new()
        .filename(&db)
        .create_if_missing(true);
    let pool: SqlitePool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(opts)
        .await
        .expect("connect");
    MIGRATIONS.run(&pool).await.expect("migrate");
    (EmbeddingStore::new(pool), dir)
}

fn key(track: &str, model: &str) -> EmbeddingKey {
    EmbeddingKey::new(TrackId::from(track), ModelVersion::from(model))
}

#[tokio::test]
async fn enqueue_then_status_is_not_started() {
    let (store, _dir) = fresh_store().await;
    let k = key("t1", "clap-v1");
    store.enqueue(&k).await.unwrap();
    assert_eq!(
        store.status(&k).await.unwrap(),
        Some(IngestStatus::NotStarted)
    );
}

#[tokio::test]
async fn enqueue_is_idempotent() {
    let (store, _dir) = fresh_store().await;
    let k = key("t1", "clap-v1");
    store.enqueue(&k).await.unwrap();
    store.enqueue(&k).await.unwrap();
    let counts = store.counts(&ModelVersion::from("clap-v1")).await.unwrap();
    assert_eq!(counts.not_started, 1);
}

#[tokio::test]
async fn claim_next_returns_none_on_empty_queue() {
    let (store, _dir) = fresh_store().await;
    let claimed = store
        .claim_next(&ModelVersion::from("clap-v1"))
        .await
        .unwrap();
    assert!(claimed.is_none());
}

#[tokio::test]
async fn claim_next_pops_oldest_in_fifo_order() {
    let (store, _dir) = fresh_store().await;
    store.enqueue(&key("a", "clap-v1")).await.unwrap();
    // Make sure created_at differs by at least 1 ms — otherwise tie-break is
    // implementation-defined and the test gets flaky.
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    store.enqueue(&key("b", "clap-v1")).await.unwrap();

    let first = store
        .claim_next(&ModelVersion::from("clap-v1"))
        .await
        .unwrap();
    let second = store
        .claim_next(&ModelVersion::from("clap-v1"))
        .await
        .unwrap();
    assert_eq!(first.unwrap().track_id.as_str(), "a");
    assert_eq!(second.unwrap().track_id.as_str(), "b");
}

#[tokio::test]
async fn claim_next_skips_other_model_versions() {
    let (store, _dir) = fresh_store().await;
    store.enqueue(&key("a", "clap-v1")).await.unwrap();
    store.enqueue(&key("b", "clap-v2")).await.unwrap();

    let claimed = store
        .claim_next(&ModelVersion::from("clap-v2"))
        .await
        .unwrap();
    assert_eq!(claimed.unwrap().track_id.as_str(), "b");
}

#[tokio::test]
async fn claim_next_transitions_status_to_in_progress() {
    let (store, _dir) = fresh_store().await;
    let k = key("t1", "clap-v1");
    store.enqueue(&k).await.unwrap();
    store
        .claim_next(&ModelVersion::from("clap-v1"))
        .await
        .unwrap();
    assert_eq!(
        store.status(&k).await.unwrap(),
        Some(IngestStatus::InProgress)
    );
}

#[tokio::test]
async fn mark_done_stores_vector_and_status() {
    let (store, _dir) = fresh_store().await;
    let k = key("t1", "clap-v1");
    store.enqueue(&k).await.unwrap();
    store
        .claim_next(&ModelVersion::from("clap-v1"))
        .await
        .unwrap();

    let v = vec![0.1f32, -0.5, 0.7, 1.0];
    let emb = Embedding::new(k.clone(), v.clone());
    store.mark_done(&emb).await.unwrap();

    assert_eq!(store.status(&k).await.unwrap(), Some(IngestStatus::Done));
    let got = store.get(&k).await.unwrap().expect("embedding");
    assert_eq!(got.vector, v);
}

#[tokio::test]
async fn get_returns_none_for_pending_rows() {
    let (store, _dir) = fresh_store().await;
    let k = key("t1", "clap-v1");
    store.enqueue(&k).await.unwrap();
    // Status is `not_started`; vector hasn't been written.
    assert!(store.get(&k).await.unwrap().is_none());
}

#[tokio::test]
async fn get_returns_none_for_unknown_key() {
    let (store, _dir) = fresh_store().await;
    assert!(store.get(&key("nope", "clap-v1")).await.unwrap().is_none());
}

#[tokio::test]
async fn model_version_isolation() {
    // Same track embedded under two different model versions: both
    // rows coexist; reading with the v1 key returns the v1 vector.
    let (store, _dir) = fresh_store().await;
    let k1 = key("t1", "clap-v1");
    let k2 = key("t1", "clap-v2");

    store.enqueue(&k1).await.unwrap();
    store.enqueue(&k2).await.unwrap();
    store
        .claim_next(&ModelVersion::from("clap-v1"))
        .await
        .unwrap();
    store
        .claim_next(&ModelVersion::from("clap-v2"))
        .await
        .unwrap();

    store
        .mark_done(&Embedding::new(k1.clone(), vec![1.0, 2.0]))
        .await
        .unwrap();
    store
        .mark_done(&Embedding::new(k2.clone(), vec![10.0, 20.0]))
        .await
        .unwrap();

    let v1 = store.get(&k1).await.unwrap().unwrap().vector;
    let v2 = store.get(&k2).await.unwrap().unwrap().vector;
    assert_eq!(v1, vec![1.0, 2.0]);
    assert_eq!(v2, vec![10.0, 20.0]);
}

#[tokio::test]
async fn mark_failed_records_error() {
    let (store, _dir) = fresh_store().await;
    let k = key("t1", "clap-v1");
    store.enqueue(&k).await.unwrap();
    store
        .claim_next(&ModelVersion::from("clap-v1"))
        .await
        .unwrap();
    store
        .mark_failed(&k, "embedder returned 503")
        .await
        .unwrap();
    assert_eq!(store.status(&k).await.unwrap(), Some(IngestStatus::Failed));
}

#[tokio::test]
async fn reset_failed_re_enqueues_failed_rows() {
    let (store, _dir) = fresh_store().await;
    let k = key("t1", "clap-v1");
    store.enqueue(&k).await.unwrap();
    store
        .claim_next(&ModelVersion::from("clap-v1"))
        .await
        .unwrap();
    store.mark_failed(&k, "boom").await.unwrap();

    let reset = store
        .reset_failed(&ModelVersion::from("clap-v1"))
        .await
        .unwrap();
    assert_eq!(reset, 1);
    assert_eq!(
        store.status(&k).await.unwrap(),
        Some(IngestStatus::NotStarted)
    );
}

#[tokio::test]
async fn reset_in_progress_recovers_crashed_workers() {
    // Simulate: a worker claimed a row and then the gateway crashed
    // before it could mark_done. On startup we run reset_in_progress
    // and the row goes back into the queue.
    let (store, _dir) = fresh_store().await;
    let k = key("t1", "clap-v1");
    store.enqueue(&k).await.unwrap();
    store
        .claim_next(&ModelVersion::from("clap-v1"))
        .await
        .unwrap();

    let reset = store.reset_in_progress().await.unwrap();
    assert_eq!(reset, 1);
    assert_eq!(
        store.status(&k).await.unwrap(),
        Some(IngestStatus::NotStarted)
    );
}

#[tokio::test]
async fn counts_aggregates_by_status() {
    let (store, _dir) = fresh_store().await;
    let mv = ModelVersion::from("clap-v1");

    store.enqueue(&key("done1", "clap-v1")).await.unwrap();
    store.enqueue(&key("done2", "clap-v1")).await.unwrap();
    store.enqueue(&key("pending", "clap-v1")).await.unwrap();
    store.enqueue(&key("failed", "clap-v1")).await.unwrap();

    // done1, done2 → done
    let k1 = key("done1", "clap-v1");
    let k2 = key("done2", "clap-v1");
    store.claim_next(&mv).await.unwrap(); // claims done1
    store.claim_next(&mv).await.unwrap(); // claims done2
    store
        .mark_done(&Embedding::new(k1, vec![0.0]))
        .await
        .unwrap();
    store
        .mark_done(&Embedding::new(k2, vec![0.0]))
        .await
        .unwrap();

    // pending stays not_started, failed transitions through in_progress → failed
    let kf = key("failed", "clap-v1");
    store.claim_next(&mv).await.unwrap(); // claims pending
    // ...but we want pending to stay pending, so claim again to grab
    // the *failed* row, then fail it.
    store.claim_next(&mv).await.unwrap(); // claims failed
    store.mark_failed(&kf, "x").await.unwrap();

    let c = store.counts(&mv).await.unwrap();
    assert_eq!(c.done, 2);
    assert_eq!(
        c.in_progress, 1,
        "the 'pending' row was claimed by accident in this fixture"
    );
    assert_eq!(c.failed, 1);
    assert_eq!(c.not_started, 0);
}

#[tokio::test]
async fn vectors_round_trip_negatives_and_subnormals() {
    let (store, _dir) = fresh_store().await;
    let k = key("t1", "clap-v1");
    store.enqueue(&k).await.unwrap();
    store
        .claim_next(&ModelVersion::from("clap-v1"))
        .await
        .unwrap();

    let v = vec![
        -0.0_f32,
        f32::MIN_POSITIVE / 2.0,
        1e-30,
        -1e30,
        std::f32::consts::PI,
    ];
    store
        .mark_done(&Embedding::new(k.clone(), v.clone()))
        .await
        .unwrap();
    let got = store.get(&k).await.unwrap().unwrap();
    assert_eq!(got.vector, v);
}

#[tokio::test]
async fn list_done_embeddings_returns_done_rows_ordered_by_track_id() {
    let (store, _dir) = fresh_store().await;
    let model = ModelVersion::from("clap-v1");

    // Two done rows (inserted out of track_id order), one still queued
    // (must be excluded), and one under a different model_version.
    for (track, vec) in [
        ("t-b", vec![3.0_f32, 4.0]),
        ("t-a", vec![1.0_f32, 2.0]),
    ] {
        let k = key(track, "clap-v1");
        store.enqueue(&k).await.unwrap();
        store.claim_next(&model).await.unwrap();
        store
            .mark_done(&Embedding::new(k, vec))
            .await
            .unwrap();
    }
    // Not-yet-embedded: enqueued but not done.
    store.enqueue(&key("t-c", "clap-v1")).await.unwrap();
    // Different model: done, but must not appear in clap-v1's list.
    let other = key("t-d", "clap-v2");
    store.enqueue(&other).await.unwrap();
    store.claim_next(&ModelVersion::from("clap-v2")).await.unwrap();
    store
        .mark_done(&Embedding::new(other, vec![9.0_f32, 9.0]))
        .await
        .unwrap();

    let got = store.list_done_embeddings(&model).await.unwrap();
    let ids: Vec<&str> = got.iter().map(|e| e.key.track_id.as_str()).collect();
    assert_eq!(ids, vec!["t-a", "t-b"]); // ordered by track_id, done-only, model-scoped
    assert_eq!(got[0].vector, vec![1.0_f32, 2.0]);
    assert_eq!(got[1].vector, vec![3.0_f32, 4.0]);
}

#[tokio::test]
async fn list_done_embeddings_empty_when_none_done() {
    let (store, _dir) = fresh_store().await;
    store.enqueue(&key("t1", "clap-v1")).await.unwrap();
    let got = store
        .list_done_embeddings(&ModelVersion::from("clap-v1"))
        .await
        .unwrap();
    assert!(got.is_empty());
}
