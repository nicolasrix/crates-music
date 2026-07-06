//! L3 audio cache: on-disk blobs + SQLite metadata.
//!
//! Keys are content-addressed by `(track_id, bitrate, codec)`. Blobs live on
//! disk under `<root>/blobs/<sha256>` (full SHA-256 → no collisions). Metadata
//! (size, last-accessed timestamp, pinned flag) lives in SQLite alongside the
//! L2 metadata cache.
//!
//! Eviction is LRU by `last_accessed_at`, bounded by `regular_budget_bytes`.
//! Pinned entries live in a separate budget and are excluded from LRU. The
//! eviction loop never deletes the only remaining row — the pragmatic choice
//! is "this single track exceeds budget, but the user just asked for it."
//!
//! Writes are atomic: tmp file + `rename`. A crash mid-write leaves a stray
//! `<sha>.tmp` that the next sweep can clean up; it never produces a
//! truncated-but-canonical blob.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};

use crate::Error;

/// Content-addressing key for a cached audio body.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AudioKey {
    pub track_id: String,
    /// Encoded bitrate in kbps. `None` means the original (un-transcoded) file.
    pub bitrate: Option<u32>,
    /// Codec name as Subsonic returns it (e.g. `"mp3"`, `"opus"`, `"flac"`).
    pub codec: String,
}

impl AudioKey {
    /// Stable, human-readable serialisation: `track|bitrate|codec`. Used as
    /// the SQLite primary key and as input to the blob filename hash.
    pub fn canonical(&self) -> String {
        let bitrate = match self.bitrate {
            Some(b) => b.to_string(),
            None => String::from("orig"),
        };
        format!("{}|{}|{}", self.track_id, bitrate, self.codec)
    }

    fn blob_filename(&self) -> String {
        use std::fmt::Write;
        let mut hasher = Sha256::new();
        hasher.update(self.canonical().as_bytes());
        let digest = hasher.finalize();
        let mut s = String::with_capacity(64);
        for byte in &digest {
            write!(&mut s, "{byte:02x}").expect("write to String never fails");
        }
        s
    }
}

#[derive(Debug, Clone)]
pub struct AudioEntry {
    pub key: AudioKey,
    pub blob_path: PathBuf,
    pub bytes: u64,
    pub last_accessed_at: SystemTime,
    pub pinned: bool,
}

/// Result of attempting to pin a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinOutcome {
    /// Newly pinned.
    Pinned,
    /// Already pinned — no-op.
    AlreadyPinned,
    /// Track is not in the cache; pin first requires a `put`.
    NotInCache,
    /// Pinning would push pinned bytes past `pinned_budget_bytes`.
    /// Argument is the byte count over budget.
    WouldExceedBudget { over_by: u64 },
}

/// Result of attempting to unpin a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnpinOutcome {
    /// Was pinned, now unpinned. The entry may have been LRU-evicted as a
    /// consequence (now-unpinned bytes count against the regular budget).
    Unpinned,
    /// Was already unpinned — no-op.
    NotPinned,
    /// Track is not in the cache.
    NotInCache,
}

#[derive(Debug, Clone, Copy)]
pub struct AudioCacheStats {
    pub regular_count: u64,
    pub regular_bytes: u64,
    pub regular_budget_bytes: u64,
    pub pinned_count: u64,
    pub pinned_bytes: u64,
    pub pinned_budget_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct AudioCache {
    root: PathBuf,
    pool: SqlitePool,
    // Budgets are `Arc<AtomicU64>` (not plain `u64`) so the running TUI's
    // Settings view can lower them at runtime through a shared `AudioCache`
    // handle — `set_budgets` takes `&self`, and every `.clone()` of the cache
    // observes the change (shared atomics, not per-clone copies).
    regular_budget_bytes: Arc<AtomicU64>,
    pinned_budget_bytes: Arc<AtomicU64>,
}

impl AudioCache {
    /// Open (or create) an audio cache rooted at `root`.
    /// The blob directory is `<root>/blobs/`, the SQLite DB is `<root>/audio.sqlite`.
    pub async fn open(
        root: &Path,
        regular_budget_bytes: u64,
        pinned_budget_bytes: u64,
    ) -> Result<Self, Error> {
        tokio::fs::create_dir_all(root).await?;
        let blobs_dir = root.join("blobs");
        tokio::fs::create_dir_all(&blobs_dir).await?;

        let db_path = root.join("audio.sqlite");
        let opts = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self {
            root: root.to_path_buf(),
            pool,
            regular_budget_bytes: Arc::new(AtomicU64::new(regular_budget_bytes)),
            pinned_budget_bytes: Arc::new(AtomicU64::new(pinned_budget_bytes)),
        })
    }

    pub fn regular_budget_bytes(&self) -> u64 {
        self.regular_budget_bytes.load(Ordering::Relaxed)
    }

    pub fn pinned_budget_bytes(&self) -> u64 {
        self.pinned_budget_bytes.load(Ordering::Relaxed)
    }

    /// Change the budgets on a live cache. The lowered regular budget takes
    /// effect on the next `put`; call [`Self::evict_lru_to_fit`] afterwards to
    /// reclaim space immediately. The pinned budget only gates future pins —
    /// already-pinned entries are never LRU-evicted.
    pub fn set_budgets(&self, regular_budget_bytes: u64, pinned_budget_bytes: u64) {
        self.regular_budget_bytes
            .store(regular_budget_bytes, Ordering::Relaxed);
        self.pinned_budget_bytes
            .store(pinned_budget_bytes, Ordering::Relaxed);
    }

    fn blob_path_for(&self, key: &AudioKey) -> PathBuf {
        self.root.join("blobs").join(key.blob_filename())
    }

    pub async fn get(&self, key: &AudioKey) -> Result<Option<AudioEntry>, Error> {
        let canonical = key.canonical();
        let row = sqlx::query(
            "SELECT key, track_id, bitrate, codec, blob_path, bytes, last_accessed_at, pinned \
             FROM audio_entries WHERE key = ?",
        )
        .bind(&canonical)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(row_to_entry))
    }

    /// Find any cached entry for a track id, regardless of `(bitrate, codec)`.
    /// A pinned entry wins, then the most-recently-accessed. Lets callers that
    /// only hold a track id (offline playback, quality-agnostic unpin) locate
    /// a blob that was cached under a *different* quality than the one they'd
    /// derive today — otherwise a track pinned at one quality becomes invisible
    /// (unplayable offline, unable to be unpinned) after the quality setting
    /// changes.
    pub async fn find_by_track(&self, track_id: &str) -> Result<Option<AudioEntry>, Error> {
        let row = sqlx::query(
            "SELECT key, track_id, bitrate, codec, blob_path, bytes, last_accessed_at, pinned \
             FROM audio_entries WHERE track_id = ? \
             ORDER BY pinned DESC, last_accessed_at DESC LIMIT 1",
        )
        .bind(track_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(row_to_entry))
    }

    /// Insert or replace a cache entry. Writes the blob atomically (tmp +
    /// rename) and triggers LRU eviction if the regular budget is exceeded.
    /// Pinned status is preserved across replacement.
    pub async fn put(&self, key: &AudioKey, body: Bytes) -> Result<AudioEntry, Error> {
        let canonical = key.canonical();
        let blob_path = self.blob_path_for(key);
        let tmp_path = blob_path.with_extension("tmp");
        tokio::fs::write(&tmp_path, body.as_ref()).await?;
        tokio::fs::rename(&tmp_path, &blob_path).await?;

        let now = SystemTime::now();
        let now_ms = systemtime_to_millis_i64(now);
        let bytes_len = body.len() as u64;
        let blob_path_str = blob_path.to_string_lossy().into_owned();

        // ON CONFLICT does NOT touch `pinned` — re-inserting a pinned track
        // leaves it pinned.
        sqlx::query(
            "INSERT INTO audio_entries \
                 (key, track_id, bitrate, codec, blob_path, bytes, last_accessed_at, pinned) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 0) \
             ON CONFLICT(key) DO UPDATE SET \
                 track_id = excluded.track_id, \
                 bitrate = excluded.bitrate, \
                 codec = excluded.codec, \
                 blob_path = excluded.blob_path, \
                 bytes = excluded.bytes, \
                 last_accessed_at = excluded.last_accessed_at",
        )
        .bind(&canonical)
        .bind(&key.track_id)
        .bind(key.bitrate)
        .bind(&key.codec)
        .bind(&blob_path_str)
        .bind(u64_to_i64(bytes_len))
        .bind(now_ms)
        .execute(&self.pool)
        .await?;

        self.evict_lru_to_fit().await?;

        // The just-inserted entry might itself be the only regular row and
        // exceed the budget; we never evict the sole remaining row, so it
        // should still exist. If a put-of-a-pinned-key was raced with an
        // unpin, the row could be gone — handle defensively by recomputing.
        match self.get(key).await? {
            Some(entry) => Ok(entry),
            None => Ok(AudioEntry {
                key: key.clone(),
                blob_path,
                bytes: bytes_len,
                last_accessed_at: now,
                pinned: false,
            }),
        }
    }

    pub async fn delete(&self, key: &AudioKey) -> Result<bool, Error> {
        let canonical = key.canonical();
        let row = sqlx::query("SELECT blob_path FROM audio_entries WHERE key = ?")
            .bind(&canonical)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let blob_path: String = row.get("blob_path");
        // Ignore NotFound: external rm shouldn't break us. Surface other IO errors.
        if let Err(e) = tokio::fs::remove_file(&blob_path).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(Error::Io(e));
        }
        sqlx::query("DELETE FROM audio_entries WHERE key = ?")
            .bind(&canonical)
            .execute(&self.pool)
            .await?;
        Ok(true)
    }

    /// Bump `last_accessed_at` to "now". Returns true iff a row matched.
    pub async fn touch(&self, key: &AudioKey) -> Result<bool, Error> {
        let now_ms = systemtime_to_millis_i64(SystemTime::now());
        let res = sqlx::query("UPDATE audio_entries SET last_accessed_at = ? WHERE key = ?")
            .bind(now_ms)
            .bind(key.canonical())
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Pin a cached track to the pinned-budget bucket. The track must already
    /// be in the cache (call `put` first). Refuses to pin if it would exceed
    /// `pinned_budget_bytes`. Idempotent: pinning an already-pinned track
    /// returns [`PinOutcome::AlreadyPinned`].
    pub async fn pin(&self, key: &AudioKey) -> Result<PinOutcome, Error> {
        let canonical = key.canonical();
        let row = sqlx::query("SELECT bytes, pinned FROM audio_entries WHERE key = ?")
            .bind(&canonical)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            return Ok(PinOutcome::NotInCache);
        };
        let bytes: i64 = row.get("bytes");
        let pinned: i64 = row.get("pinned");
        if pinned != 0 {
            return Ok(PinOutcome::AlreadyPinned);
        }
        let entry_bytes = i64_to_u64(bytes);

        // Budget check: would the new pinned total exceed pinned_budget_bytes?
        let pinned_total: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(bytes), 0) FROM audio_entries WHERE pinned = 1",
        )
        .fetch_one(&self.pool)
        .await?;
        let projected = i64_to_u64(pinned_total).saturating_add(entry_bytes);
        let pinned_budget = self.pinned_budget_bytes();
        if projected > pinned_budget {
            let over_by = projected - pinned_budget;
            return Ok(PinOutcome::WouldExceedBudget { over_by });
        }

        sqlx::query("UPDATE audio_entries SET pinned = 1 WHERE key = ?")
            .bind(&canonical)
            .execute(&self.pool)
            .await?;
        Ok(PinOutcome::Pinned)
    }

    /// Unpin a track. The entry returns to the regular bucket and may be
    /// immediately LRU-evicted if that pushes regular bytes over budget.
    pub async fn unpin(&self, key: &AudioKey) -> Result<UnpinOutcome, Error> {
        let canonical = key.canonical();
        let row = sqlx::query("SELECT pinned FROM audio_entries WHERE key = ?")
            .bind(&canonical)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            return Ok(UnpinOutcome::NotInCache);
        };
        let pinned: i64 = row.get("pinned");
        if pinned == 0 {
            return Ok(UnpinOutcome::NotPinned);
        }
        sqlx::query("UPDATE audio_entries SET pinned = 0 WHERE key = ?")
            .bind(&canonical)
            .execute(&self.pool)
            .await?;
        // Newly-unpinned bytes count against regular budget — may need eviction.
        self.evict_lru_to_fit().await?;
        Ok(UnpinOutcome::Unpinned)
    }

    /// All currently-pinned entries, ordered by `track_id` for stable output.
    pub async fn list_pinned(&self) -> Result<Vec<AudioEntry>, Error> {
        let rows = sqlx::query(
            "SELECT key, track_id, bitrate, codec, blob_path, bytes, last_accessed_at, pinned \
             FROM audio_entries WHERE pinned = 1 ORDER BY track_id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(row_to_entry).collect())
    }

    pub async fn stats(&self) -> Result<AudioCacheStats, Error> {
        let row = sqlx::query(
            "SELECT \
                 COALESCE(SUM(CASE WHEN pinned = 0 THEN 1 ELSE 0 END), 0) AS reg_count, \
                 COALESCE(SUM(CASE WHEN pinned = 0 THEN bytes ELSE 0 END), 0) AS reg_bytes, \
                 COALESCE(SUM(CASE WHEN pinned = 1 THEN 1 ELSE 0 END), 0) AS pin_count, \
                 COALESCE(SUM(CASE WHEN pinned = 1 THEN bytes ELSE 0 END), 0) AS pin_bytes \
             FROM audio_entries",
        )
        .fetch_one(&self.pool)
        .await?;
        let reg_count: i64 = row.get("reg_count");
        let reg_bytes: i64 = row.get("reg_bytes");
        let pin_count: i64 = row.get("pin_count");
        let pin_bytes: i64 = row.get("pin_bytes");
        Ok(AudioCacheStats {
            regular_count: i64_to_u64(reg_count),
            regular_bytes: i64_to_u64(reg_bytes),
            regular_budget_bytes: self.regular_budget_bytes(),
            pinned_count: i64_to_u64(pin_count),
            pinned_bytes: i64_to_u64(pin_bytes),
            pinned_budget_bytes: self.pinned_budget_bytes(),
        })
    }

    /// Evict LRU regular entries until total regular bytes ≤ budget.
    /// Never evicts the only remaining row. Returns the final regular byte total.
    pub async fn evict_lru_to_fit(&self) -> Result<u64, Error> {
        loop {
            let total: i64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(bytes), 0) FROM audio_entries WHERE pinned = 0",
            )
            .fetch_one(&self.pool)
            .await?;
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM audio_entries WHERE pinned = 0")
                    .fetch_one(&self.pool)
                    .await?;
            let total_u64 = i64_to_u64(total);
            if total_u64 <= self.regular_budget_bytes() || count <= 1 {
                return Ok(total_u64);
            }

            let row = sqlx::query(
                "SELECT key, blob_path FROM audio_entries \
                 WHERE pinned = 0 \
                 ORDER BY last_accessed_at ASC, key ASC LIMIT 1",
            )
            .fetch_optional(&self.pool)
            .await?;
            let Some(row) = row else {
                return Ok(total_u64);
            };
            let key: String = row.get("key");
            let blob_path: String = row.get("blob_path");
            if let Err(e) = tokio::fs::remove_file(&blob_path).await
                && e.kind() != std::io::ErrorKind::NotFound
            {
                return Err(Error::Io(e));
            }
            sqlx::query("DELETE FROM audio_entries WHERE key = ?")
                .bind(&key)
                .execute(&self.pool)
                .await?;
        }
    }
}

fn row_to_entry(row: &SqliteRow) -> AudioEntry {
    let _key_canonical: String = row.get("key");
    let track_id: String = row.get("track_id");
    let bitrate: Option<i64> = row.get("bitrate");
    let codec: String = row.get("codec");
    let blob_path: String = row.get("blob_path");
    let bytes: i64 = row.get("bytes");
    let last_accessed_at_ms: i64 = row.get("last_accessed_at");
    let pinned: i64 = row.get("pinned");
    AudioEntry {
        key: AudioKey {
            track_id,
            bitrate: bitrate.and_then(|b| u32::try_from(b).ok()),
            codec,
        },
        blob_path: PathBuf::from(blob_path),
        bytes: i64_to_u64(bytes),
        last_accessed_at: UNIX_EPOCH
            + std::time::Duration::from_millis(i64_to_u64(last_accessed_at_ms)),
        pinned: pinned != 0,
    }
}

fn systemtime_to_millis_i64(t: SystemTime) -> i64 {
    let ms = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    i64::try_from(ms).unwrap_or(i64::MAX)
}

fn u64_to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn i64_to_u64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}
