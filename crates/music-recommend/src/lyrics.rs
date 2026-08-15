//! Per-track lyrics cache — the storage half of the lyrics feature.
//!
//! Lives here, next to the other stores over
//! `gateway-state.recommend.sqlite`, because that is where the table is
//! (migration `0023_track_lyrics.sql`) and because the lookup key an
//! external provider needs — artist, title, album, duration — is already
//! in [`crate::metadata::track_metadata`]'s pool. Resolution policy
//! (which provider, in what order, with what timeouts) is deliberately
//! *not* here; that is the gateway's business. This module only knows how
//! to read and write a resolved answer.
//!
//! Two properties are load-bearing for the layers above:
//!
//! * A [`LyricsSource::None`] row is a real, stored answer — the negative
//!   cache. Without it, every play of an un-lyriced track re-hits the
//!   provider. It is written only when a lookup *completed* and found
//!   nothing, never when one failed to run.
//! * [`Self::get`] returns expired rows. Expiry is advice for the
//!   resolver, not a delete: an expired hit is still the best thing to
//!   serve when the provider is unreachable (stale-if-error).

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use music_core::TrackId;

use crate::Result;

/// Where a cached answer came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LyricsSource {
    /// The file's own tags or an `.lrc` sidecar, via Navidrome. Ground
    /// truth for that file — a user who edited or re-timed the tags meant
    /// it, so this always outranks an external match.
    Navidrome,
    /// An external community database.
    Lrclib,
    /// Looked, found nothing. Distinct from "never looked", which is the
    /// absence of a row.
    None,
}

impl LyricsSource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            LyricsSource::Navidrome => "navidrome",
            LyricsSource::Lrclib => "lrclib",
            LyricsSource::None => "none",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "navidrome" => LyricsSource::Navidrome,
            "lrclib" => LyricsSource::Lrclib,
            // An unrecognised source is treated as a miss rather than
            // rejected: a row written by a newer build must not make an
            // older one fail to read its own cache.
            _ => LyricsSource::None,
        }
    }
}

/// How confidently an external hit was matched. Kept as an audit trail —
/// when the wrong song's lyrics show up, this says which tier to
/// distrust.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchKind {
    /// Artist + title + album + duration all supplied.
    Exact,
    /// Album dropped. Tag albums drift ("Deluxe Edition", "Remastered"),
    /// so an album mismatch is weak evidence of a *song* mismatch.
    NoAlbum,
    /// Fuzzy search, accepted only under a duration guard.
    Search,
}

impl MatchKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            MatchKind::Exact => "exact",
            MatchKind::NoAlbum => "no_album",
            MatchKind::Search => "search",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "exact" => Some(MatchKind::Exact),
            "no_album" => Some(MatchKind::NoAlbum),
            "search" => Some(MatchKind::Search),
            _ => None,
        }
    }
}

/// One timed lyric line. `start_ms` is absolute from the start of the
/// track — any provider-side offset is folded in before storage, so a
/// consumer never applies one itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricLine {
    pub start_ms: i64,
    pub text: String,
}

/// A resolved (or resolved-as-absent) lyrics answer for one track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LyricsRow {
    pub track_id: TrackId,
    pub source: LyricsSource,
    pub match_kind: Option<MatchKind>,
    pub synced: bool,
    /// The provider states the track has no words. A *successful* lookup
    /// with a definitive answer — the client shows "Instrumental", not a
    /// retry affordance.
    pub instrumental: bool,
    /// Timestamp-free text, populated whenever any words were found —
    /// including for synced hits, so a static view never has to strip
    /// timestamps itself.
    pub plain_text: Option<String>,
    /// Ordered by `start_ms`. Empty unless `synced`.
    pub lines: Vec<LyricLine>,
    pub provider_id: Option<String>,
    pub fetched_at: i64,
    pub expires_at: i64,
}

impl LyricsRow {
    /// A miss that has been *looked up and confirmed absent* — the
    /// negative-cache row.
    #[must_use]
    pub fn miss(track_id: TrackId, now_ms: i64, ttl_ms: i64) -> Self {
        Self {
            track_id,
            source: LyricsSource::None,
            match_kind: None,
            synced: false,
            instrumental: false,
            plain_text: None,
            lines: Vec::new(),
            provider_id: None,
            fetched_at: now_ms,
            expires_at: now_ms.saturating_add(ttl_ms),
        }
    }

    #[must_use]
    pub fn is_expired(&self, now_ms: i64) -> bool {
        now_ms >= self.expires_at
    }

    /// Whether this row carries anything worth showing. `false` for both
    /// a miss and an instrumental-with-no-text.
    #[must_use]
    pub fn has_text(&self) -> bool {
        !self.lines.is_empty() || self.plain_text.is_some()
    }
}

/// SQLite-backed lyrics cache. Shares its pool with [`crate::EmbeddingStore`];
/// construct from `EmbeddingStore::pool().clone()`.
#[derive(Clone, Debug)]
pub struct LyricsStore {
    pool: SqlitePool,
}

impl LyricsStore {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// The cached answer for `track_id`, **expired or not** — see the
    /// module note on stale-if-error. `None` means "never looked".
    #[tracing::instrument(name = "lyrics.get", skip(self), fields(track = %track_id))]
    pub async fn get(&self, track_id: &TrackId) -> Result<Option<LyricsRow>> {
        let row = sqlx::query(
            "SELECT track_id, source, match_kind, synced, instrumental, plain_text, \
                    lines_json, provider_id, fetched_at, expires_at \
             FROM track_lyrics WHERE track_id = ?",
        )
        .bind(track_id.as_str())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(row_to_lyrics))
    }

    /// Insert or replace the answer for `row.track_id`. Last write wins:
    /// a re-resolution is always more current than what it replaces.
    #[tracing::instrument(
        name = "lyrics.upsert",
        skip(self, row),
        fields(track = %row.track_id, source = row.source.as_str(), synced = row.synced)
    )]
    pub async fn upsert(&self, row: &LyricsRow) -> Result<()> {
        // Serializing an owned Vec of plain structs cannot fail; the
        // fallback keeps that impossibility from becoming a panic.
        let lines_json = if row.lines.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&row.lines).unwrap_or_else(|_| "[]".to_string()))
        };
        sqlx::query(
            "INSERT INTO track_lyrics \
                 (track_id, source, match_kind, synced, instrumental, plain_text, \
                  lines_json, provider_id, fetched_at, expires_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (track_id) DO UPDATE SET \
                 source = excluded.source, \
                 match_kind = excluded.match_kind, \
                 synced = excluded.synced, \
                 instrumental = excluded.instrumental, \
                 plain_text = excluded.plain_text, \
                 lines_json = excluded.lines_json, \
                 provider_id = excluded.provider_id, \
                 fetched_at = excluded.fetched_at, \
                 expires_at = excluded.expires_at",
        )
        .bind(row.track_id.as_str())
        .bind(row.source.as_str())
        .bind(row.match_kind.map(MatchKind::as_str))
        .bind(i64::from(row.synced))
        .bind(i64::from(row.instrumental))
        .bind(row.plain_text.as_deref())
        .bind(lines_json)
        .bind(row.provider_id.as_deref())
        .bind(row.fetched_at)
        .bind(row.expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Drop the cached answer, so the next read re-resolves from scratch.
    /// The escape hatch behind the client's "wrong lyrics" control.
    pub async fn delete(&self, track_id: &TrackId) -> Result<()> {
        sqlx::query("DELETE FROM track_lyrics WHERE track_id = ?")
            .bind(track_id.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Row counts grouped by `source`, for diagnostics ("how much of the
    /// library actually has timed lyrics?").
    pub async fn counts_by_source(&self) -> Result<Vec<(String, i64)>> {
        let rows = sqlx::query(
            "SELECT source, COUNT(*) AS n FROM track_lyrics GROUP BY source ORDER BY n DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<String, _>("source"), r.get::<i64, _>("n")))
            .collect())
    }
}

fn row_to_lyrics(row: &sqlx::sqlite::SqliteRow) -> LyricsRow {
    let lines = row
        .get::<Option<String>, _>("lines_json")
        .and_then(|json| serde_json::from_str::<Vec<LyricLine>>(&json).ok())
        .unwrap_or_default();
    LyricsRow {
        track_id: TrackId::from(row.get::<String, _>("track_id")),
        source: LyricsSource::parse(&row.get::<String, _>("source")),
        match_kind: row
            .get::<Option<String>, _>("match_kind")
            .as_deref()
            .and_then(MatchKind::parse),
        synced: row.get::<i64, _>("synced") != 0,
        instrumental: row.get::<i64, _>("instrumental") != 0,
        plain_text: row.get("plain_text"),
        lines,
        provider_id: row.get("provider_id"),
        fetched_at: row.get("fetched_at"),
        expires_at: row.get("expires_at"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::EmbeddingStore;

    async fn store() -> (LyricsStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recommend.sqlite");
        let embeddings = EmbeddingStore::open(&path).await.unwrap();
        (LyricsStore::new(embeddings.pool().clone()), dir)
    }

    fn synced_row(track: &str, now: i64) -> LyricsRow {
        LyricsRow {
            track_id: TrackId::from(track.to_string()),
            source: LyricsSource::Lrclib,
            match_kind: Some(MatchKind::Exact),
            synced: true,
            instrumental: false,
            plain_text: Some("one\ntwo".to_string()),
            lines: vec![
                LyricLine { start_ms: 1000, text: "one".to_string() },
                LyricLine { start_ms: 4500, text: "two".to_string() },
            ],
            provider_id: Some("12345".to_string()),
            fetched_at: now,
            expires_at: now + 1000,
        }
    }

    #[tokio::test]
    async fn round_trips_a_synced_hit() {
        let (store, _dir) = store().await;
        let row = synced_row("tr-1", 1_000_000);
        store.upsert(&row).await.unwrap();
        let got = store.get(&row.track_id).await.unwrap().unwrap();
        assert_eq!(got, row);
    }

    #[tokio::test]
    async fn miss_row_is_the_negative_cache() {
        let (store, _dir) = store().await;
        let id = TrackId::from("tr-miss".to_string());
        let row = LyricsRow::miss(id.clone(), 1_000, 5_000);
        store.upsert(&row).await.unwrap();

        let got = store.get(&id).await.unwrap().unwrap();
        assert_eq!(got.source, LyricsSource::None);
        assert!(!got.has_text());
        // "Looked and found nothing" is distinguishable from "never
        // looked" — the latter is the absent row below.
        assert!(store
            .get(&TrackId::from("tr-never".to_string()))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn expired_rows_are_returned_not_hidden() {
        let (store, _dir) = store().await;
        let row = synced_row("tr-stale", 0);
        store.upsert(&row).await.unwrap();

        let got = store.get(&row.track_id).await.unwrap().unwrap();
        // Well past expires_at, yet still served: the resolver decides
        // whether staleness matters, and a stale hit beats nothing when
        // the provider is down.
        assert!(got.is_expired(9_999_999));
        assert_eq!(got.lines.len(), 2);
    }

    #[tokio::test]
    async fn upsert_replaces_and_counts_by_source() {
        let (store, _dir) = store().await;
        let id = TrackId::from("tr-1".to_string());
        store
            .upsert(&LyricsRow::miss(id.clone(), 0, 100))
            .await
            .unwrap();
        // A later successful resolution must win over the earlier miss.
        store.upsert(&synced_row("tr-1", 500)).await.unwrap();

        let got = store.get(&id).await.unwrap().unwrap();
        assert_eq!(got.source, LyricsSource::Lrclib);
        assert!(got.synced);

        store
            .upsert(&LyricsRow::miss(TrackId::from("tr-2".to_string()), 0, 100))
            .await
            .unwrap();
        let counts = store.counts_by_source().await.unwrap();
        assert_eq!(counts.len(), 2);
        assert!(counts.contains(&("lrclib".to_string(), 1)));
        assert!(counts.contains(&("none".to_string(), 1)));
    }

    #[tokio::test]
    async fn delete_forces_a_fresh_resolution() {
        let (store, _dir) = store().await;
        let row = synced_row("tr-bad-match", 0);
        store.upsert(&row).await.unwrap();
        store.delete(&row.track_id).await.unwrap();
        assert!(store.get(&row.track_id).await.unwrap().is_none());
    }
}
