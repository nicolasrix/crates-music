//! SQLite-backed gateway-owned playlist store.
//!
//! Lives in the same `gateway-state.sqlite` pool as the `users`/OAuth
//! tables (migration `0007_playlists.sql`), so `owner_user_id`'s foreign
//! key and `ON DELETE CASCADE` work without a cross-file reference. The
//! store is pure id-plumbing: it holds Navidrome **track ids** and never
//! touches catalog metadata — clients hydrate ids → tracks against the
//! shared `/rest/*` catalog. That keeps the ownership boundary clean
//! (gateway owns the *membership*, Navidrome owns the *tracks*).

use std::collections::HashSet;
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::RngCore;
use sqlx::{Row, SqlitePool};

/// A playlist row plus its derived track count. `owned` is filled per
/// request from the caller's id, so the same row is `owned=true` for its
/// owner and `owned=false` when surfaced to another user as `shared`.
#[derive(Debug, Clone)]
pub struct PlaylistRow {
    pub id: String,
    pub owner_user_id: i64,
    pub name: String,
    pub visibility: String,
    pub song_count: i64,
    pub created_ms: i64,
    pub updated_ms: i64,
}

/// How `set_tracks` applies the supplied id list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackMode {
    /// Replace the whole membership (used for reorder + full set).
    Replace,
    /// Append after the current last position (used for "add to playlist").
    /// Ids already in the playlist are **skipped**, not duplicated.
    Append,
}

/// What a membership write actually did. `skipped` is only ever non-zero
/// under [`TrackMode::Append`] — it's the count the client needs to tell
/// the user "that song is already in this playlist".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackWrite {
    pub added: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone)]
pub struct PlaylistStore {
    pool: SqlitePool,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// 128-bit random id, lowercase hex. Plenty of entropy to never collide at
/// single-household scale; ordering is carried by `created_ms`, not the id,
/// so we don't need the time-prefixing of a uuid-v7.
fn new_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().fold(String::with_capacity(32), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

impl PlaylistStore {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Create an empty playlist owned by `owner_user_id`.
    pub async fn create(&self, owner_user_id: i64, name: &str) -> Result<PlaylistRow, sqlx::Error> {
        let id = new_id();
        let now = now_ms();
        sqlx::query(
            "INSERT INTO playlists (id, owner_user_id, name, visibility, created_ms, updated_ms) \
             VALUES (?, ?, ?, 'private', ?, ?)",
        )
        .bind(&id)
        .bind(owner_user_id)
        .bind(name)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(PlaylistRow {
            id,
            owner_user_id,
            name: name.to_string(),
            visibility: "private".to_string(),
            song_count: 0,
            created_ms: now,
            updated_ms: now,
        })
    }

    /// Playlists the caller may see: their own (any visibility) plus other
    /// users' `shared` ones. Newest first. Song counts come from a single
    /// grouped join so the list stays one round-trip.
    pub async fn list_visible(&self, user_id: i64) -> Result<Vec<PlaylistRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT p.id, p.owner_user_id, p.name, p.visibility, p.created_ms, p.updated_ms, \
                    COUNT(pt.track_id) AS song_count \
             FROM playlists p \
             LEFT JOIN playlist_tracks pt ON pt.playlist_id = p.id \
             WHERE p.owner_user_id = ? OR p.visibility = 'shared' \
             GROUP BY p.id \
             ORDER BY p.updated_ms DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(row_to_playlist).collect())
    }

    /// Fetch one playlist row. Visibility/ownership is the handler's call —
    /// this is a pure read.
    pub async fn get(&self, id: &str) -> Result<Option<PlaylistRow>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT p.id, p.owner_user_id, p.name, p.visibility, p.created_ms, p.updated_ms, \
                    COUNT(pt.track_id) AS song_count \
             FROM playlists p \
             LEFT JOIN playlist_tracks pt ON pt.playlist_id = p.id \
             WHERE p.id = ? \
             GROUP BY p.id",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(row_to_playlist))
    }

    /// Ordered track ids of a playlist.
    pub async fn track_ids(&self, id: &str) -> Result<Vec<String>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT track_id FROM playlist_tracks WHERE playlist_id = ? ORDER BY position ASC",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.get::<String, _>("track_id")).collect())
    }

    /// Update name and/or visibility. `None` leaves a field unchanged.
    /// Bumps `updated_ms`. Returns the refreshed row (or `None` if gone).
    pub async fn patch(
        &self,
        id: &str,
        name: Option<&str>,
        visibility: Option<&str>,
    ) -> Result<Option<PlaylistRow>, sqlx::Error> {
        let now = now_ms();
        sqlx::query(
            "UPDATE playlists SET \
                name = COALESCE(?, name), \
                visibility = COALESCE(?, visibility), \
                updated_ms = ? \
             WHERE id = ?",
        )
        .bind(name)
        .bind(visibility)
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        self.get(id).await
    }

    /// Set membership. `Replace` swaps the whole list (reorder + set);
    /// `Append` adds after the current tail, skipping ids the playlist
    /// already holds. Positions are dense 0..n. Runs in a transaction so a
    /// half-written reorder can never persist — and so the append-mode
    /// duplicate check can't race a concurrent add from another device.
    pub async fn set_tracks(
        &self,
        id: &str,
        track_ids: &[String],
        mode: TrackMode,
    ) -> Result<TrackWrite, sqlx::Error> {
        let mut tx = self.pool.begin().await?;

        // Both arms hand back the start position plus exactly what to
        // insert, so the loop below doesn't care which mode produced it.
        let (start, to_insert): (i64, Vec<&String>) = match mode {
            TrackMode::Replace => {
                sqlx::query("DELETE FROM playlist_tracks WHERE playlist_id = ?")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                (0, track_ids.iter().collect())
            }
            TrackMode::Append => {
                let row = sqlx::query(
                    "SELECT COALESCE(MAX(position) + 1, 0) AS next FROM playlist_tracks \
                     WHERE playlist_id = ?",
                )
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
                let existing =
                    sqlx::query("SELECT track_id FROM playlist_tracks WHERE playlist_id = ?")
                        .bind(id)
                        .fetch_all(&mut *tx)
                        .await?;
                // Seed the seen-set from current membership; `insert`
                // returning false then rejects repeats *within* the batch
                // too — an album listing the same id twice is one add.
                let mut seen: HashSet<&str> =
                    existing.iter().map(|r| r.get::<&str, _>("track_id")).collect();
                let fresh = track_ids.iter().filter(|t| seen.insert(t.as_str())).collect();
                (row.get::<i64, _>("next"), fresh)
            }
        };

        for (offset, track_id) in to_insert.iter().enumerate() {
            let position = start + i64::try_from(offset).unwrap_or(i64::MAX);
            sqlx::query(
                "INSERT INTO playlist_tracks (playlist_id, position, track_id) VALUES (?, ?, ?)",
            )
            .bind(id)
            .bind(position)
            .bind(track_id)
            .execute(&mut *tx)
            .await?;
        }

        // An append that added nothing (every id was already a member)
        // left the playlist untouched — don't bump `updated_ms` and shuffle
        // it to the top of the "newest first" list for a no-op.
        let added = to_insert.len();
        if added > 0 || mode == TrackMode::Replace {
            sqlx::query("UPDATE playlists SET updated_ms = ? WHERE id = ?")
                .bind(now_ms())
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        Ok(TrackWrite { added, skipped: track_ids.len() - added })
    }

    /// Delete a playlist. `playlist_tracks` rows cascade.
    pub async fn delete(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM playlists WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

fn row_to_playlist(row: &sqlx::sqlite::SqliteRow) -> PlaylistRow {
    PlaylistRow {
        id: row.get("id"),
        owner_user_id: row.get("owner_user_id"),
        name: row.get("name"),
        visibility: row.get("visibility"),
        song_count: row.get("song_count"),
        created_ms: row.get("created_ms"),
        updated_ms: row.get("updated_ms"),
    }
}
