//! L2 metadata cache: SQLite-backed, ETag-keyed, content-addressed.
//!
//! Public API kept narrow: open / get / put / delete / expire_before. ETags
//! are the first 16 hex chars of SHA-256(body) so they are stable across
//! processes and across restarts — see [`etag_for`].

#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

pub mod audio;

pub use audio::{AudioCache, AudioCacheStats, AudioEntry, AudioKey, PinOutcome, UnpinOutcome};

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub etag: String,
    pub body: Bytes,
    pub fetched_at: SystemTime,
    pub ttl: Duration,
}

impl Entry {
    /// Whether the entry is still within its TTL relative to `now`.
    pub fn is_fresh(&self, now: SystemTime) -> bool {
        match now.duration_since(self.fetched_at) {
            Ok(elapsed) => elapsed < self.ttl,
            Err(_) => true, // `now` predates `fetched_at` → treat as fresh
        }
    }
}

/// SHA-256(body), truncated to 16 hex chars. Stable, content-addressed,
/// short enough to fit comfortably in HTTP headers.
pub fn etag_for(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    let digest = hasher.finalize();
    let mut s = String::with_capacity(16);
    for byte in &digest[..8] {
        use std::fmt::Write;
        write!(&mut s, "{byte:02x}").expect("write to String never fails");
    }
    s
}

#[derive(Debug, Clone)]
pub struct Cache {
    pool: SqlitePool,
}

impl Cache {
    /// Open an in-memory cache. Tests use this; production never should —
    /// it disappears with the process.
    pub async fn open_in_memory() -> Result<Self> {
        let opts = SqliteConnectOptions::new()
            .in_memory(true)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            // In-memory SQLite is single-connection only — multiple connections
            // each get their own private DB.
            .max_connections(1)
            .connect_with(opts)
            .await?;
        run_migrations(&pool).await?;
        Ok(Self { pool })
    }

    /// Open (or create) a file-backed cache.
    pub async fn open(path: &Path) -> Result<Self> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?;
        run_migrations(&pool).await?;
        Ok(Self { pool })
    }

    #[tracing::instrument(
        name = "cache.lookup",
        skip_all,
        fields(hit = tracing::field::Empty),
    )]
    pub async fn get(&self, key: &str) -> Result<Option<Entry>> {
        let row = sqlx::query(
            "SELECT key, etag, body, fetched_at, ttl_seconds \
             FROM cache_entries WHERE key = ?",
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;
        let entry = row.as_ref().map(row_to_entry);
        tracing::Span::current().record("hit", entry.is_some());
        Ok(entry)
    }

    #[tracing::instrument(
        name = "cache.write",
        skip_all,
        fields(bytes = body.len()),
    )]
    pub async fn put(&self, key: &str, body: Bytes, ttl: Duration) -> Result<Entry> {
        let now = SystemTime::now();
        self.put_at(key, body, now, ttl).await
    }

    /// Like `put` but with an explicit `fetched_at`. Used by `insert_for_test`
    /// to construct stale entries with a controlled timestamp.
    async fn put_at(
        &self,
        key: &str,
        body: Bytes,
        fetched_at: SystemTime,
        ttl: Duration,
    ) -> Result<Entry> {
        let etag = etag_for(&body);
        let fetched_at_secs = systemtime_to_secs_i64(fetched_at);
        let ttl_secs = duration_to_secs_i64(ttl);
        sqlx::query(
            "INSERT INTO cache_entries (key, etag, body, fetched_at, ttl_seconds) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(key) DO UPDATE SET \
             etag = excluded.etag, \
             body = excluded.body, \
             fetched_at = excluded.fetched_at, \
             ttl_seconds = excluded.ttl_seconds",
        )
        .bind(key)
        .bind(&etag)
        .bind(body.as_ref())
        .bind(fetched_at_secs)
        .bind(ttl_secs)
        .execute(&self.pool)
        .await?;

        Ok(Entry {
            key: key.to_string(),
            etag,
            body,
            fetched_at,
            ttl,
        })
    }

    pub async fn delete(&self, key: &str) -> Result<bool> {
        let res = sqlx::query("DELETE FROM cache_entries WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Remove every entry whose deadline (`fetched_at + ttl`) is at or before
    /// `cutoff`. Returns the number of rows removed.
    pub async fn expire_before(&self, cutoff: SystemTime) -> Result<u64> {
        let cutoff_secs = systemtime_to_secs_i64(cutoff);
        let res = sqlx::query(
            "DELETE FROM cache_entries \
             WHERE fetched_at + ttl_seconds <= ?",
        )
        .bind(cutoff_secs)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Test-only helper for seeding entries with arbitrary `fetched_at`.
    /// Kept on the public surface (with a `_for_test` suffix) so integration
    /// tests in this crate can call it without reaching into private items.
    #[doc(hidden)]
    pub async fn insert_for_test(
        &self,
        key: &str,
        body: Bytes,
        fetched_at: SystemTime,
        ttl: Duration,
    ) -> Result<Entry> {
        self.put_at(key, body, fetched_at, ttl).await
    }
}

async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

fn row_to_entry(row: &sqlx::sqlite::SqliteRow) -> Entry {
    let key: String = row.get("key");
    let etag: String = row.get("etag");
    let body_blob: Vec<u8> = row.get("body");
    let fetched_at_secs: i64 = row.get("fetched_at");
    let ttl_secs: i64 = row.get("ttl_seconds");
    Entry {
        key,
        etag,
        body: Bytes::from(body_blob),
        fetched_at: UNIX_EPOCH + Duration::from_secs(secs_i64_to_u64(fetched_at_secs)),
        ttl: Duration::from_secs(secs_i64_to_u64(ttl_secs)),
    }
}

fn systemtime_to_secs_i64(t: SystemTime) -> i64 {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    i64::try_from(secs).unwrap_or(i64::MAX)
}

fn duration_to_secs_i64(d: Duration) -> i64 {
    i64::try_from(d.as_secs()).unwrap_or(i64::MAX)
}

fn secs_i64_to_u64(secs: i64) -> u64 {
    u64::try_from(secs).unwrap_or(0)
}
