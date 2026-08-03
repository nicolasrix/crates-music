//! SQLite-backed embedding store.
//!
//! The store is intentionally thin: it owns table layout and atomic
//! status transitions, but knows nothing about audio, models, or the
//! ANN index. Callers compose those.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use music_core::TrackId;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::types::{Embedding, EmbeddingKey, IngestStatus, ModelVersion};
use crate::{Error, Result};

/// Migrations owned by the embedding store. Each crate owns its own
/// SQLite file so migration version numbers don't collide across the
/// workspace.
pub static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Rows per transaction in [`EmbeddingStore::enqueue_many`]. Bounds how
/// long a bulk enqueue holds SQLite's write lock against the concurrent
/// ingest workers; 1000 inserts is a few milliseconds, comfortably
/// inside the pool's `busy_timeout`.
const ENQUEUE_CHUNK: usize = 1000;

#[derive(Clone, Debug)]
pub struct EmbeddingStore {
    pool: SqlitePool,
}

impl EmbeddingStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Open a file-backed store and run migrations. Creates the file
    /// if it doesn't exist.
    ///
    /// We deliberately give the recommender its own SQLite file rather
    /// than layering onto the OAuth pool — sqlx's migration runner
    /// uses monotonic version numbers across the whole pool, which
    /// makes "many crates contributing migrations" fragile. One file
    /// per crate keeps each migration directory self-contained.
    pub async fn open(path: &Path) -> Result<Self> {
        // WAL + busy_timeout is required for concurrent ingest workers.
        // Default journal_mode (`delete`) serializes writers and errors
        // out on lock contention; WAL allows one writer alongside many
        // readers, and the timeout makes brief contentions wait
        // instead of failing.
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await?;
        MIGRATIONS.run(&pool).await?;
        Ok(Self { pool })
    }

    /// In-memory store. Tests use this; the single-connection pool
    /// is required because in-memory SQLite isn't shared across
    /// connections.
    pub async fn open_in_memory() -> Result<Self> {
        let opts = SqliteConnectOptions::new()
            .in_memory(true)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        MIGRATIONS.run(&pool).await?;
        Ok(Self { pool })
    }

    /// Direct pool access for adjacent code paths (rebuild_ann_from_store).
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Enqueue a track for ingest if no row exists yet for this
    /// `(track_id, model_version)`. No-op if a row is already present
    /// in any state (`done` won't be re-queued, `failed` requires
    /// explicit retry via [`Self::reset_failed`]).
    pub async fn enqueue(&self, key: &EmbeddingKey) -> Result<()> {
        let now = now_ms();
        sqlx::query(
            "INSERT OR IGNORE INTO track_embeddings
                 (track_id, model_version, dim, vector, status, error, created_at, updated_at)
             VALUES (?, ?, 0, NULL, 'not_started', NULL, ?, ?)",
        )
        .bind(key.track_id.as_str())
        .bind(key.model_version.as_str())
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Enqueue many tracks for one model version. Returns the number of
    /// rows *actually inserted* — i.e. tracks that had no row yet, which
    /// is exactly the "newly discovered" count the catalog watcher wants
    /// to log.
    ///
    /// Same `INSERT OR IGNORE` semantics as [`Self::enqueue`], so it is
    /// safe to hand it the whole catalog on every scan: already-known
    /// tracks (in any state, including `done` and `failed`) are ignored
    /// and don't inflate the return value.
    ///
    /// Batched rather than N autocommits, because the discovery sweep
    /// passes ~10⁴ ids and a per-statement fsync each would turn a few
    /// milliseconds of work into tens of seconds of disk.
    ///
    /// Chunked rather than one giant transaction, because a write
    /// transaction blocks the ingest workers' `claim_next`.
    /// `ENQUEUE_CHUNK` keeps each lock window to a few milliseconds —
    /// well inside the pool's `busy_timeout` — instead of holding the
    /// write lock for as long as it takes to walk the whole catalog. A
    /// failure part-way leaves earlier chunks committed, which is fine:
    /// the operation is idempotent, so the next sweep finishes the job.
    pub async fn enqueue_many(
        &self,
        track_ids: &[TrackId],
        model_version: &ModelVersion,
    ) -> Result<u64> {
        let now = now_ms();
        let mut inserted = 0u64;
        for chunk in track_ids.chunks(ENQUEUE_CHUNK) {
            let mut tx = self.pool.begin().await?;
            for track_id in chunk {
                let r = sqlx::query(
                    "INSERT OR IGNORE INTO track_embeddings
                         (track_id, model_version, dim, vector, status, error, created_at, updated_at)
                     VALUES (?, ?, 0, NULL, 'not_started', NULL, ?, ?)",
                )
                .bind(track_id.as_str())
                .bind(model_version.as_str())
                .bind(now)
                .bind(now)
                .execute(&mut *tx)
                .await?;
                inserted += r.rows_affected();
            }
            tx.commit().await?;
        }
        Ok(inserted)
    }

    /// Atomically claim the oldest `not_started` row for the given
    /// model_version, transitioning it to `in_progress`. Returns the
    /// claimed key, or `None` if the queue is empty.
    ///
    /// Implementation note: SQLite doesn't support `RETURNING` reliably
    /// Single atomic statement; safe for many concurrent workers.
    ///
    /// The previous shape used a transaction with separate SELECT and
    /// UPDATE. With `pool.begin()` (BEGIN DEFERRED), two workers each
    /// acquire a read lock during SELECT, then deadlock when both try
    /// to upgrade to write — SQLite returns SQLITE_BUSY immediately
    /// regardless of busy_timeout, because timeouts only cover waiting
    /// on a *held* lock, not lock-upgrade contention. UPDATE…RETURNING
    /// (SQLite ≥ 3.35) sidesteps that by acquiring the write lock up
    /// front and selecting the row in the same statement.
    #[tracing::instrument(name = "store.claim_next", skip(self), fields(model = %model_version))]
    pub async fn claim_next(&self, model_version: &ModelVersion) -> Result<Option<EmbeddingKey>> {
        let now = now_ms();
        let row = sqlx::query(
            "UPDATE track_embeddings
                SET status = 'in_progress', updated_at = ?
              WHERE rowid = (
                  SELECT rowid FROM track_embeddings
                   WHERE model_version = ? AND status = 'not_started'
                   ORDER BY created_at ASC
                   LIMIT 1
              )
              RETURNING track_id",
        )
        .bind(now)
        .bind(model_version.as_str())
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        let track_id: String = row.get("track_id");

        Ok(Some(EmbeddingKey {
            track_id: TrackId::from(track_id),
            model_version: model_version.clone(),
        }))
    }

    /// Mark an in-progress row as done and store its embedding vector.
    #[tracing::instrument(name = "store.mark_done", skip(self, embedding), fields(track = %embedding.key.track_id))]
    pub async fn mark_done(&self, embedding: &Embedding) -> Result<()> {
        let dim = i64::try_from(embedding.dim()).expect("vector dim fits in i64");
        let blob = vector_to_blob(&embedding.vector);
        let now = now_ms();
        sqlx::query(
            "UPDATE track_embeddings
                SET status = 'done', dim = ?, vector = ?, error = NULL, updated_at = ?
              WHERE track_id = ? AND model_version = ?",
        )
        .bind(dim)
        .bind(blob)
        .bind(now)
        .bind(embedding.key.track_id.as_str())
        .bind(embedding.key.model_version.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark an in-progress (or any) row as failed with an error message.
    /// Retryable via [`Self::reset_failed`].
    pub async fn mark_failed(&self, key: &EmbeddingKey, error: &str) -> Result<()> {
        let now = now_ms();
        sqlx::query(
            "UPDATE track_embeddings
                SET status = 'failed', error = ?, updated_at = ?
              WHERE track_id = ? AND model_version = ?",
        )
        .bind(error)
        .bind(now)
        .bind(key.track_id.as_str())
        .bind(key.model_version.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Move every `failed` row for the given model_version back to
    /// `not_started` so the worker re-attempts. Useful after fixing the
    /// embedder.
    pub async fn reset_failed(&self, model_version: &ModelVersion) -> Result<u64> {
        let now = now_ms();
        let r = sqlx::query(
            "UPDATE track_embeddings
                SET status = 'not_started', error = NULL, updated_at = ?
              WHERE model_version = ? AND status = 'failed'",
        )
        .bind(now)
        .bind(model_version.as_str())
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// On gateway startup, any rows still flagged `in_progress` belong
    /// to a worker that crashed mid-job — reset them back to
    /// `not_started` so they get retried.
    pub async fn reset_in_progress(&self) -> Result<u64> {
        let now = now_ms();
        let r = sqlx::query(
            "UPDATE track_embeddings
                SET status = 'not_started', updated_at = ?
              WHERE status = 'in_progress'",
        )
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// Look up the embedding for a `(track_id, model_version)` pair.
    /// Returns `None` if no row, or if the row is still pending /
    /// failed (no usable vector).
    pub async fn get(&self, key: &EmbeddingKey) -> Result<Option<Embedding>> {
        let row = sqlx::query(
            "SELECT vector, dim, status FROM track_embeddings
              WHERE track_id = ? AND model_version = ?",
        )
        .bind(key.track_id.as_str())
        .bind(key.model_version.as_str())
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else { return Ok(None) };
        let status: String = row.get("status");
        if status != "done" {
            return Ok(None);
        }
        let dim: i64 = row.get("dim");
        let blob: Vec<u8> = row.get("vector");
        let vector = blob_to_vector(&blob)?;
        let dim_usize = usize::try_from(dim).expect("dim fits in usize");
        if vector.len() != dim_usize {
            return Err(Error::DimMismatch {
                stored: dim_usize,
                got: vector.len(),
            });
        }
        Ok(Some(Embedding {
            key: key.clone(),
            vector,
        }))
    }

    /// All `done` embeddings for `model_version`, ordered by `track_id`
    /// for stable iteration. Used by the auto-projection task, which
    /// ships the whole matrix to the embedder for reduction. Returns an
    /// empty vec when nothing is ready (caller decides what that means).
    ///
    /// A corrupt or dim-mismatched row is a hard error rather than a
    /// silent skip — a bad vector in the projection input would distort
    /// the whole layout, so fail loudly and let the caller log it.
    pub async fn list_done_embeddings(
        &self,
        model_version: &ModelVersion,
    ) -> Result<Vec<Embedding>> {
        let rows = sqlx::query(
            "SELECT track_id, dim, vector FROM track_embeddings
              WHERE model_version = ? AND status = 'done' AND vector IS NOT NULL
              ORDER BY track_id",
        )
        .bind(model_version.as_str())
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let track_id: String = row.get("track_id");
            let dim: i64 = row.get("dim");
            let blob: Vec<u8> = row.get("vector");
            let vector = blob_to_vector(&blob)?;
            let dim_usize = usize::try_from(dim).expect("dim fits in usize");
            if vector.len() != dim_usize {
                return Err(Error::DimMismatch {
                    stored: dim_usize,
                    got: vector.len(),
                });
            }
            out.push(Embedding {
                key: EmbeddingKey::new(track_id, model_version.clone()),
                vector,
            });
        }
        Ok(out)
    }

    /// Status of a `(track_id, model_version)` row, or `None` if no row.
    pub async fn status(&self, key: &EmbeddingKey) -> Result<Option<IngestStatus>> {
        let row = sqlx::query(
            "SELECT status FROM track_embeddings WHERE track_id = ? AND model_version = ?",
        )
        .bind(key.track_id.as_str())
        .bind(key.model_version.as_str())
        .fetch_optional(&self.pool)
        .await?;
        match row {
            None => Ok(None),
            Some(r) => {
                let s: String = r.get("status");
                Ok(Some(IngestStatus::parse(&s)?))
            }
        }
    }

    /// Count rows for the given model_version, grouped by status.
    /// Useful for `/v1/recommend/health` and ops dashboards.
    pub async fn counts(&self, model_version: &ModelVersion) -> Result<StatusCounts> {
        let rows = sqlx::query(
            "SELECT status, COUNT(*) AS n FROM track_embeddings
              WHERE model_version = ?
              GROUP BY status",
        )
        .bind(model_version.as_str())
        .fetch_all(&self.pool)
        .await?;

        let mut counts = StatusCounts::default();
        for row in rows {
            let status: String = row.get("status");
            let n: i64 = row.get("n");
            let n = u64::try_from(n.max(0)).unwrap_or(0);
            match status.as_str() {
                "not_started" => counts.not_started = n,
                "in_progress" => counts.in_progress = n,
                "done" => counts.done = n,
                "failed" => counts.failed = n,
                _ => {}
            }
        }
        Ok(counts)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatusCounts {
    pub not_started: u64,
    pub in_progress: u64,
    pub done: u64,
    pub failed: u64,
}

fn now_ms() -> i64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

pub(crate) fn vector_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

pub(crate) fn blob_to_vector(b: &[u8]) -> Result<Vec<f32>> {
    if !b.len().is_multiple_of(4) {
        return Err(Error::CorruptVectorBlob { bytes: b.len() });
    }
    let mut out = Vec::with_capacity(b.len() / 4);
    for chunk in b.chunks_exact(4) {
        out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(out)
}
