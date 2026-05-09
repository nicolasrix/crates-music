//! SQLite-backed ring buffer for closed `tracing` spans.
//!
//! Three operations matter on the hot path:
//!   * `insert_batch` — drainer flushes accumulated spans.
//!   * `trim_to_capacity` — bounds disk usage.
//!   * `recent(n)` — diagnostics UI reads the latest spans.
//!
//! All three are scoped to a single table; the store knows nothing
//! about model versions, embedder probes, or HTTP. Higher layers
//! compose on top.

use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use super::types::SpanRecord;

/// Migrations live in their own directory so they don't collide with
/// the OAuth migrations also owned by this crate. sqlx tracks the
/// applied set per-pool, but two migrators against the *same* DB file
/// would interleave — splitting directories keeps the two state
/// databases (OAuth, diagnostics) cleanly independent.
static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations-diagnostics");

#[derive(Clone, Debug)]
pub struct TraceStore {
    pool: SqlitePool,
}

impl TraceStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Open a file-backed traces DB and run migrations. WAL +
    /// busy_timeout matches the rest of the project — we expect the
    /// drainer task to be the only writer, but a future diagnostics
    /// HTTP handler may also write (e.g. browser RUM uploads).
    pub async fn open(path: &Path) -> sqlx::Result<Self> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?;
        MIGRATIONS.run(&pool).await?;
        Ok(Self { pool })
    }

    /// In-memory variant for tests. Single-connection pool because
    /// in-memory SQLite isn't shared across connections.
    pub async fn open_in_memory() -> sqlx::Result<Self> {
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

    pub async fn count(&self) -> sqlx::Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM spans")
            .fetch_one(&self.pool)
            .await?;
        row.try_get::<i64, _>("n")
    }

    /// Persist a batch of closed spans in a single transaction.
    /// No-op (and not an error) on an empty batch — the drainer will
    /// flush every tick whether or not there's data.
    pub async fn insert_batch(&self, batch: Vec<SpanRecord>) -> sqlx::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for s in &batch {
            sqlx::query(
                "INSERT INTO spans
                    (trace_id, span_id, parent_span_id, name, target,
                     start_ms, end_ms, fields_json)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&s.trace_id)
            .bind(s.span_id)
            .bind(s.parent_span_id)
            .bind(&s.name)
            .bind(&s.target)
            .bind(s.start_ms)
            .bind(s.end_ms)
            .bind(&s.fields_json)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Return the most recently inserted spans, newest first. Ordered
    /// by `id` (autoincrement) — that's "most recently received" in
    /// the layer, which matches what the diagnostics page wants. Note
    /// this is NOT ordered by `end_ms`: a long-running span that
    /// closes late but started early should still float to the top of
    /// the feed when it lands.
    pub async fn recent(&self, limit: usize) -> sqlx::Result<Vec<SpanRecord>> {
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(
            "SELECT trace_id, span_id, parent_span_id, name, target,
                    start_ms, end_ms, fields_json
             FROM spans
             ORDER BY id DESC
             LIMIT ?",
        )
        .bind(limit_i64)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(SpanRecord {
                trace_id: row.try_get("trace_id")?,
                span_id: row.try_get("span_id")?,
                parent_span_id: row.try_get("parent_span_id")?,
                name: row.try_get("name")?,
                target: row.try_get("target")?,
                start_ms: row.try_get("start_ms")?,
                end_ms: row.try_get("end_ms")?,
                fields_json: row.try_get("fields_json")?,
            });
        }
        Ok(out)
    }

    /// Evict oldest rows so at most `max_rows` remain. The drainer
    /// calls this after every flush. Cheap when already under
    /// capacity (single MAX(id) read + a no-op DELETE).
    pub async fn trim_to_capacity(&self, max_rows: usize) -> sqlx::Result<()> {
        let max_rows_i64 = i64::try_from(max_rows).unwrap_or(i64::MAX);
        // Subquery uses MAX(id), not COUNT(*) — autoincrement means
        // "rows survive iff their id > (max_id - max_rows)" is the
        // exact frontier we want, even after past trims.
        sqlx::query(
            "DELETE FROM spans
             WHERE id <= COALESCE((SELECT MAX(id) FROM spans), 0) - ?",
        )
        .bind(max_rows_i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
