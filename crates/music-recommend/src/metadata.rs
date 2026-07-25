//! Track metadata cache (artist, album, title, etc.) keyed by `track_id`.
//!
//! Sibling to the embedding store: lives in the same SQLite file
//! (`gateway-state.recommend.sqlite`) so that joins of the form
//! "for these N similar track_ids, give me artists for diversity rerank"
//! cost a single query.
//!
//! Metadata is **not** model-versioned the way embeddings are — the
//! mapping `track_id → metadata` doesn't depend on which CLAP checkpoint
//! is loaded. Re-tagging a file in Navidrome regenerates `track_id`,
//! which means old rows orphan rather than going stale; the orphan is
//! never queried because the embedding for the same id is also gone.
//!
//! Population paths:
//!   1. **Ingest worker side-effect** — every track that gets embedded
//!      also gets metadata fetched (one cheap `getSong` next to the
//!      already-paid multi-MB audio fetch).
//!   2. **Lazy on-miss** — recommend handlers spawn a background fetch
//!      for any unknown `track_id` they surface. Self-heals over time.
//!   3. **One-shot backfill** — gateway flag walks embedded-but-uncached
//!      track_ids on first deploy.
//!
//! This module owns paths (1) only as far as the store API; the worker
//! hook lives in `ingest.rs`.

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use music_core::TrackId;
use sqlx::{Row, SqlitePool};

use crate::Result;

/// Title-normalization for cross-edition dedup keys.
///
/// The goal: two recordings of the same song under different editions
/// (remaster, live, acoustic, deluxe-bundle, …) should hash to the same
/// `(artist_norm, title_norm)` tuple so we can pick a single
/// representative when surfacing recommendations.
///
/// **Conservative on purpose.** False-positive collapses (treating
/// "Title (Part 2)" as the same as "Title") would silently drop
/// candidates from the recommendation pool. So we only strip a
/// parenthesized suffix when its inner text contains one of an
/// allowlist of edition-keywords:
///
///   remaster, live, acoustic, edit, version, feat, featuring,
///   with, deluxe, expanded
///
/// Anything else (Reprise, Part 2, Interlude, Demo, Mono) stays intact.
///
/// Also: lowercase, trim, collapse internal whitespace. Idempotent —
/// `normalize_title(normalize_title(x))` == `normalize_title(x)`.
pub fn normalize_title(s: &str) -> String {
    const EDITION_KEYWORDS: &[&str] = &[
        "remaster",
        "remastered",
        "live",
        "acoustic",
        "edit",
        "version",
        "feat",
        "featuring",
        "with",
        "deluxe",
        "expanded",
    ];

    let mut work = s.to_lowercase();

    // Strip trailing edition suffixes, possibly multiple in a row.
    // "Song (Live) (Remastered)" → strip rightmost match, then re-check.
    loop {
        let trimmed = work.trim_end();
        if !trimmed.ends_with(')') {
            break;
        }
        let Some(open_idx) = trimmed.rfind('(') else {
            break;
        };
        // close_idx is the position of the trailing ')' in `trimmed`.
        let close_idx = trimmed.len() - 1;
        let inner = &trimmed[open_idx + 1..close_idx];
        let has_keyword = EDITION_KEYWORDS.iter().any(|kw| {
            inner
                .split(|c: char| !c.is_alphanumeric())
                .any(|tok| tok == *kw)
        });
        if !has_keyword {
            break;
        }
        // Strip the suffix and continue.
        work = trimmed[..open_idx].trim_end().to_string();
    }

    // Collapse internal whitespace runs to single spaces, trim.
    let mut out = String::with_capacity(work.len());
    let mut last_was_ws = true; // suppress leading whitespace
    for ch in work.chars() {
        if ch.is_whitespace() {
            if !last_was_ws {
                out.push(' ');
                last_was_ws = true;
            }
        } else {
            out.push(ch);
            last_was_ws = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// One row in the metadata cache. Cherry-picked subset of Subsonic's
/// `child` shape — enough for diversity/dedup; not a full mirror.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackMetadata {
    pub track_id: TrackId,
    pub artist_id: Option<String>,
    pub artist: String,
    pub album_id: Option<String>,
    pub album: Option<String>,
    pub title: String,
    /// Derived from `title` via [`normalize_title`]. Stored on disk so
    /// callers can build dedup queries without re-normalizing.
    pub title_normalized: String,
    pub duration_seconds: Option<u32>,
    pub genre: Option<String>,
    pub year: Option<i32>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub bpm: Option<u32>,
    pub musical_key: Option<String>,
}

/// SQLite-backed metadata cache. Shares its connection pool with
/// [`crate::EmbeddingStore`] — they live in the same DB file so joins
/// are cheap; migrations run once for both.
#[derive(Clone, Debug)]
pub struct MetadataStore {
    pool: SqlitePool,
}

impl MetadataStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Direct pool access for adjacent code paths.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Insert or replace the metadata row for `m.track_id`. Last write
    /// wins. `created_at` is preserved across upserts; `updated_at`
    /// always advances.
    #[tracing::instrument(name = "metadata.upsert", skip(self, m), fields(track = %m.track_id))]
    pub async fn upsert(&self, m: &TrackMetadata) -> Result<()> {
        let now = now_ms();
        // SQLite's UPSERT (ON CONFLICT DO UPDATE) preserves created_at
        // by only re-binding it on the INSERT path; the UPDATE path
        // omits it.
        sqlx::query(
            "INSERT INTO track_metadata
                 (track_id, artist_id, artist, album_id, album, title, title_normalized,
                  duration_seconds, genre, year, track_number, disc_number, bpm, musical_key,
                  created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(track_id) DO UPDATE SET
                 artist_id        = excluded.artist_id,
                 artist           = excluded.artist,
                 album_id         = excluded.album_id,
                 album            = excluded.album,
                 title            = excluded.title,
                 title_normalized = excluded.title_normalized,
                 duration_seconds = excluded.duration_seconds,
                 genre            = excluded.genre,
                 year             = excluded.year,
                 track_number     = excluded.track_number,
                 disc_number      = excluded.disc_number,
                 bpm              = excluded.bpm,
                 musical_key      = excluded.musical_key,
                 updated_at       = excluded.updated_at",
        )
        .bind(m.track_id.as_str())
        .bind(m.artist_id.as_deref())
        .bind(&m.artist)
        .bind(m.album_id.as_deref())
        .bind(m.album.as_deref())
        .bind(&m.title)
        .bind(&m.title_normalized)
        .bind(m.duration_seconds.map(i64::from))
        .bind(m.genre.as_deref())
        .bind(m.year.map(i64::from))
        .bind(m.track_number.map(i64::from))
        .bind(m.disc_number.map(i64::from))
        .bind(m.bpm.map(i64::from))
        .bind(m.musical_key.as_deref())
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get(&self, track_id: &TrackId) -> Result<Option<TrackMetadata>> {
        let row = sqlx::query(
            "SELECT track_id, artist_id, artist, album_id, album, title, title_normalized,
                    duration_seconds, genre, year, track_number, disc_number, bpm, musical_key
               FROM track_metadata
              WHERE track_id = ?",
        )
        .bind(track_id.as_str())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(row_to_metadata))
    }

    /// Bulk lookup. Result keyed by `TrackId`; missing ids are absent
    /// from the map. Used by recommend handlers to fold artist/title
    /// onto a result list in one query rather than N.
    pub async fn get_many(&self, ids: &[TrackId]) -> Result<HashMap<TrackId, TrackMetadata>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        // SQLite parameter limit is 999 by default; chunk to be safe.
        let mut out = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(500) {
            let placeholders: String = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT track_id, artist_id, artist, album_id, album, title, title_normalized,
                        duration_seconds, genre, year, track_number, disc_number, bpm, musical_key
                   FROM track_metadata
                  WHERE track_id IN ({placeholders})"
            );
            let mut q = sqlx::query(&sql);
            for id in chunk {
                q = q.bind(id.as_str());
            }
            for row in q.fetch_all(&self.pool).await? {
                let m = row_to_metadata(&row);
                out.insert(m.track_id.clone(), m);
            }
        }
        Ok(out)
    }

    /// Track ids belonging to any of the given album ids. Powers the
    /// dislike-album → exclude-its-tracks expansion in the recommender:
    /// a disliked album excludes every track we have cached for it. Uses
    /// the `track_metadata_album_id_idx` index; chunked under SQLite's
    /// 999-parameter limit. Result deduped via the `HashSet`.
    pub async fn track_ids_for_albums(&self, album_ids: &[String]) -> Result<HashSet<TrackId>> {
        self.track_ids_for_parent("album_id", album_ids).await
    }

    /// Track ids belonging to any of the given artist ids — the
    /// dislike-artist → exclude-its-tracks expansion. Uses the
    /// `track_metadata_artist_id_idx` index.
    pub async fn track_ids_for_artists(&self, artist_ids: &[String]) -> Result<HashSet<TrackId>> {
        self.track_ids_for_parent("artist_id", artist_ids).await
    }

    /// Shared body for [`Self::track_ids_for_albums`] /
    /// [`Self::track_ids_for_artists`]. `column` is a fixed, internal
    /// literal (`"album_id"` / `"artist_id"`) — never user input — so
    /// interpolating it into the SQL is safe; the ids themselves are bound
    /// parameters.
    async fn track_ids_for_parent(
        &self,
        column: &str,
        parent_ids: &[String],
    ) -> Result<HashSet<TrackId>> {
        if parent_ids.is_empty() {
            return Ok(HashSet::new());
        }
        let mut out = HashSet::new();
        for chunk in parent_ids.chunks(500) {
            let placeholders: String = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql =
                format!("SELECT track_id FROM track_metadata WHERE {column} IN ({placeholders})");
            let mut q = sqlx::query(&sql);
            for id in chunk {
                q = q.bind(id);
            }
            for row in q.fetch_all(&self.pool).await? {
                out.insert(TrackId::from(row.get::<String, _>("track_id")));
            }
        }
        Ok(out)
    }

    /// Of the given ids, return those *not* in the cache. Drives the
    /// backfill loop and the lazy-on-miss path.
    pub async fn missing_ids(&self, ids: &[TrackId]) -> Result<Vec<TrackId>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let present = self.get_many(ids).await?;
        Ok(ids
            .iter()
            .filter(|id| !present.contains_key(*id))
            .cloned()
            .collect())
    }

    /// Find `track_id`s that are present as `done` embeddings under
    /// the given `model_version` but absent from the metadata cache.
    /// Drives the backfill walk.
    ///
    /// Cap defends against a misconfigured backfill on a huge SQLite
    /// returning megabytes in one query; callers walk chunks repeatedly
    /// until the result is shorter than `limit`.
    pub async fn missing_for_model(
        &self,
        model_version: &crate::types::ModelVersion,
        limit: i64,
    ) -> Result<Vec<TrackId>> {
        // Anti-join on the same DB via a single round-trip; cheaper
        // than `SELECT done` + `missing_ids` in two passes.
        let rows = sqlx::query(
            "SELECT te.track_id
               FROM track_embeddings te
          LEFT JOIN track_metadata  tm ON tm.track_id = te.track_id
              WHERE te.model_version = ?
                AND te.status = 'done'
                AND tm.track_id IS NULL
              ORDER BY te.track_id
              LIMIT ?",
        )
        .bind(model_version.as_str())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let id: String = r.get("track_id");
                TrackId::from(id)
            })
            .collect())
    }

    /// Track ids for which the metadata row exists but `genre` is
    /// null. Powers the one-shot backfill that ran after extending the
    /// Subsonic decoder to capture the `genre` tag — pre-fix rows are
    /// otherwise stuck without a genre until they're re-ingested.
    ///
    /// Ordered by `track_id` for a stable resume point if the backfill
    /// loop is interrupted and re-run.
    pub async fn null_genre_ids(&self, limit: i64) -> Result<Vec<TrackId>> {
        let rows = sqlx::query(
            "SELECT track_id
               FROM track_metadata
              WHERE genre IS NULL
              ORDER BY track_id
              LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let id: String = r.get("track_id");
                TrackId::from(id)
            })
            .collect())
    }

    /// Total row count. Useful for `/v1/recommend/health` and ops.
    pub async fn count(&self) -> Result<u64> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM track_metadata")
            .fetch_one(&self.pool)
            .await?;
        let n: i64 = row.get("n");
        Ok(u64::try_from(n.max(0)).unwrap_or(0))
    }
}

/// One-shot backfill: walk `done` embedding rows missing a metadata
/// entry under `model_version`, fetch their metadata via `fetcher`, and
/// upsert. Idempotent — run twice and the second pass does nothing.
///
/// Failures in `fetcher.fetch_metadata` are logged and skipped; the
/// row stays missing and the lazy-on-miss path can retry later. Failures
/// in `store.upsert` (SQLite-level) propagate — those indicate
/// infrastructure rot the caller should know about.
///
/// Batches the SELECT to `BATCH_SIZE` rows per round-trip; loops until
/// the result is smaller than the batch (i.e. exhausted). Since each
/// successful upsert removes the row from the missing set, the loop
/// always makes progress unless the fetcher consistently errors — in
/// which case we'd loop forever, so we also stop after the first batch
/// that produces zero successful upserts.
pub async fn backfill_metadata(
    store: &MetadataStore,
    fetcher: &dyn crate::ingest::MetadataFetcher,
    model_version: &crate::types::ModelVersion,
) -> Result<BackfillStats> {
    const BATCH_SIZE: i64 = 200;
    let mut stats = BackfillStats::default();
    loop {
        let missing = store.missing_for_model(model_version, BATCH_SIZE).await?;
        if missing.is_empty() {
            return Ok(stats);
        }
        let mut batch_upserts = 0u64;
        for track_id in &missing {
            match fetcher.fetch_metadata(track_id).await {
                Ok(m) => {
                    store.upsert(&m).await?;
                    stats.upserted += 1;
                    batch_upserts += 1;
                }
                Err(e) => {
                    tracing::debug!(
                        track = %track_id,
                        error = %e,
                        "backfill: metadata fetch failed; skipping"
                    );
                    stats.fetch_failed += 1;
                }
            }
        }
        // Anti-livelock: if a whole batch failed every fetch, the
        // missing set didn't shrink and the next iteration would walk
        // the same rows again. Bail.
        if batch_upserts == 0 {
            return Ok(stats);
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackfillStats {
    /// Rows successfully fetched + persisted in this run.
    pub upserted: u64,
    /// Track ids whose `fetch_metadata` returned an error. Gives the
    /// caller a metric to log so silent partial backfills are visible.
    pub fetch_failed: u64,
}

fn row_to_metadata(row: &sqlx::sqlite::SqliteRow) -> TrackMetadata {
    let track_id: String = row.get("track_id");
    let artist_id: Option<String> = row.get("artist_id");
    let artist: String = row.get("artist");
    let album_id: Option<String> = row.get("album_id");
    let album: Option<String> = row.get("album");
    let title: String = row.get("title");
    let title_normalized: String = row.get("title_normalized");
    let duration_seconds: Option<i64> = row.get("duration_seconds");
    let genre: Option<String> = row.get("genre");
    let year: Option<i64> = row.get("year");
    let track_number: Option<i64> = row.get("track_number");
    let disc_number: Option<i64> = row.get("disc_number");
    let bpm: Option<i64> = row.get("bpm");
    let musical_key: Option<String> = row.get("musical_key");
    TrackMetadata {
        track_id: TrackId::from(track_id),
        artist_id,
        artist,
        album_id,
        album,
        title,
        title_normalized,
        duration_seconds: duration_seconds.and_then(|v| u32::try_from(v).ok()),
        genre,
        year: year.and_then(|v| i32::try_from(v).ok()),
        track_number: track_number.and_then(|v| u32::try_from(v).ok()),
        disc_number: disc_number.and_then(|v| u32::try_from(v).ok()),
        bpm: bpm.and_then(|v| u32::try_from(v).ok()),
        musical_key,
    }
}

fn now_ms() -> i64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- normalize_title: identity / casing / whitespace ---

    #[test]
    fn unchanged_simple_title_is_lowercased() {
        assert_eq!(normalize_title("Bohemian Rhapsody"), "bohemian rhapsody");
    }

    #[test]
    fn already_lowercase_passes_through() {
        assert_eq!(normalize_title("imagine"), "imagine");
    }

    #[test]
    fn trims_outer_whitespace() {
        assert_eq!(normalize_title("   Hey Jude   "), "hey jude");
    }

    #[test]
    fn collapses_internal_whitespace() {
        assert_eq!(normalize_title("Hey   Jude\t\nNow"), "hey jude now");
    }

    // --- normalize_title: edition suffix stripping ---

    #[test]
    fn strips_remastered_year_suffix() {
        assert_eq!(
            normalize_title("Bohemian Rhapsody (Remastered 2011)"),
            "bohemian rhapsody"
        );
    }

    #[test]
    fn strips_remaster_without_year() {
        assert_eq!(normalize_title("Hey Jude (Remaster)"), "hey jude");
    }

    #[test]
    fn strips_live_at_venue() {
        assert_eq!(
            normalize_title("Bohemian Rhapsody (Live at Wembley)"),
            "bohemian rhapsody"
        );
    }

    #[test]
    fn strips_acoustic_version() {
        assert_eq!(
            normalize_title("Wonderwall (Acoustic Version)"),
            "wonderwall"
        );
    }

    #[test]
    fn strips_feat_collaborator() {
        assert_eq!(normalize_title("Imagine (feat. John Lennon)"), "imagine");
    }

    #[test]
    fn strips_featuring_long_form() {
        assert_eq!(
            normalize_title("Drop It (featuring Some Artist)"),
            "drop it"
        );
    }

    #[test]
    fn strips_deluxe_edition_suffix() {
        assert_eq!(
            normalize_title("Album Track (Deluxe Edition)"),
            "album track"
        );
    }

    #[test]
    fn strips_expanded_edition() {
        assert_eq!(
            normalize_title("Long Player (Expanded Edition)"),
            "long player"
        );
    }

    #[test]
    fn case_insensitive_keyword_match() {
        assert_eq!(normalize_title("TRACK (REMASTERED)"), "track");
        assert_eq!(normalize_title("Track (LIVE at Wembley)"), "track");
    }

    #[test]
    fn strips_multiple_trailing_edition_suffixes() {
        assert_eq!(normalize_title("Song (Live) (Remastered)"), "song");
    }

    // --- normalize_title: things we DO NOT strip ---

    #[test]
    fn does_not_strip_part_n_suffix() {
        // "(Part 2)" has no edition keyword — leave it alone.
        assert_eq!(normalize_title("Title (Part 2)"), "title (part 2)");
    }

    #[test]
    fn does_not_strip_reprise() {
        assert_eq!(normalize_title("Theme (Reprise)"), "theme (reprise)");
    }

    #[test]
    fn does_not_strip_interlude() {
        assert_eq!(normalize_title("Track (Interlude)"), "track (interlude)");
    }

    #[test]
    fn does_not_strip_middle_parens() {
        // Only trailing parens get stripped.
        assert_eq!(
            normalize_title("Run (Like Hell) Boy"),
            "run (like hell) boy"
        );
    }

    #[test]
    fn does_not_strip_middle_paren_even_with_keyword() {
        // "live" is in the middle, not the end — keep the title intact.
        assert_eq!(normalize_title("Live and Let Die"), "live and let die");
    }

    // --- normalize_title: edge cases ---

    #[test]
    fn empty_string_normalizes_to_empty() {
        assert_eq!(normalize_title(""), "");
    }

    #[test]
    fn whitespace_only_normalizes_to_empty() {
        assert_eq!(normalize_title("   \t\n "), "");
    }

    #[test]
    fn unbalanced_paren_does_nothing_dangerous() {
        // No matching '(' for the trailing ')' — fall through, leave it.
        // (This input shouldn't happen in practice; just don't crash.)
        assert_eq!(normalize_title("Weird Title)"), "weird title)");
    }

    #[test]
    fn idempotent_on_simple_title() {
        let once = normalize_title("Bohemian Rhapsody");
        let twice = normalize_title(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn idempotent_after_stripping() {
        let once = normalize_title("Bohemian Rhapsody (Remastered 2011)");
        let twice = normalize_title(&once);
        assert_eq!(once, twice);
        assert_eq!(once, "bohemian rhapsody");
    }

    #[test]
    fn keyword_substring_match_does_not_overstrip() {
        // "edit" is a keyword. "editorial" is NOT — keyword tokens are
        // matched as whole words (split on non-alphanumeric), so a
        // genuine-word "editorial" inside parens doesn't trigger.
        assert_eq!(
            normalize_title("Track (Editorial Choice)"),
            "track (editorial choice)"
        );
    }

    #[test]
    fn keyword_inside_parens_with_extra_text_strips() {
        // Single-edition pattern: "(Live in Paris 1995)" — keyword
        // appears as a token, strip.
        assert_eq!(
            normalize_title("Some Song (Live in Paris 1995)"),
            "some song"
        );
    }

    // --- MetadataStore ---

    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    async fn test_pool() -> SqlitePool {
        let opts = SqliteConnectOptions::new()
            .in_memory(true)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        crate::store::MIGRATIONS.run(&pool).await.unwrap();
        pool
    }

    fn sample_metadata(id: &str) -> TrackMetadata {
        TrackMetadata {
            track_id: TrackId::from(id),
            artist_id: Some("ar1".into()),
            artist: "Queen".into(),
            album_id: Some("al1".into()),
            album: Some("A Night at the Opera".into()),
            title: "Bohemian Rhapsody".into(),
            title_normalized: normalize_title("Bohemian Rhapsody"),
            duration_seconds: Some(354),
            genre: Some("Rock".into()),
            year: Some(1975),
            track_number: Some(11),
            disc_number: Some(1),
            bpm: Some(72),
            musical_key: Some("Bb".into()),
        }
    }

    #[tokio::test]
    async fn upsert_then_get_round_trips_all_fields() {
        let store = MetadataStore::new(test_pool().await);
        let m = sample_metadata("t1");
        store.upsert(&m).await.unwrap();
        let got = store.get(&m.track_id).await.unwrap().unwrap();
        assert_eq!(got, m);
    }

    #[tokio::test]
    async fn get_returns_none_for_missing_id() {
        let store = MetadataStore::new(test_pool().await);
        let got = store.get(&TrackId::from("nope")).await.unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn upsert_overwrites_existing_row() {
        let store = MetadataStore::new(test_pool().await);
        let mut m = sample_metadata("t1");
        store.upsert(&m).await.unwrap();
        m.title = "Killer Queen".into();
        m.title_normalized = normalize_title(&m.title);
        store.upsert(&m).await.unwrap();
        let got = store.get(&m.track_id).await.unwrap().unwrap();
        assert_eq!(got.title, "Killer Queen");
        assert_eq!(got.title_normalized, "killer queen");
        // count is still 1 — UPSERT didn't dupe.
        assert_eq!(store.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn upsert_handles_optional_fields_as_null() {
        let store = MetadataStore::new(test_pool().await);
        let m = TrackMetadata {
            track_id: TrackId::from("t-min"),
            artist_id: None,
            artist: "Unknown".into(),
            album_id: None,
            album: None,
            title: "Untitled".into(),
            title_normalized: normalize_title("Untitled"),
            duration_seconds: None,
            genre: None,
            year: None,
            track_number: None,
            disc_number: None,
            bpm: None,
            musical_key: None,
        };
        store.upsert(&m).await.unwrap();
        let got = store.get(&m.track_id).await.unwrap().unwrap();
        assert_eq!(got, m);
    }

    #[tokio::test]
    async fn get_many_returns_only_present_rows() {
        let store = MetadataStore::new(test_pool().await);
        store.upsert(&sample_metadata("t1")).await.unwrap();
        store.upsert(&sample_metadata("t2")).await.unwrap();
        let ids = vec![
            TrackId::from("t1"),
            TrackId::from("nope"),
            TrackId::from("t2"),
        ];
        let map = store.get_many(&ids).await.unwrap();
        assert_eq!(map.len(), 2);
        assert!(map.contains_key(&TrackId::from("t1")));
        assert!(map.contains_key(&TrackId::from("t2")));
        assert!(!map.contains_key(&TrackId::from("nope")));
    }

    #[tokio::test]
    async fn get_many_empty_input_returns_empty() {
        let store = MetadataStore::new(test_pool().await);
        let map = store.get_many(&[]).await.unwrap();
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn missing_ids_returns_difference_in_order() {
        let store = MetadataStore::new(test_pool().await);
        store.upsert(&sample_metadata("t1")).await.unwrap();
        let ids = vec![
            TrackId::from("nope1"),
            TrackId::from("t1"),
            TrackId::from("nope2"),
        ];
        let missing = store.missing_ids(&ids).await.unwrap();
        assert_eq!(
            missing,
            vec![TrackId::from("nope1"), TrackId::from("nope2")]
        );
    }

    #[tokio::test]
    async fn missing_ids_empty_input_is_empty() {
        let store = MetadataStore::new(test_pool().await);
        let missing = store.missing_ids(&[]).await.unwrap();
        assert!(missing.is_empty());
    }

    #[tokio::test]
    async fn count_starts_at_zero_and_grows() {
        let store = MetadataStore::new(test_pool().await);
        assert_eq!(store.count().await.unwrap(), 0);
        store.upsert(&sample_metadata("t1")).await.unwrap();
        store.upsert(&sample_metadata("t2")).await.unwrap();
        assert_eq!(store.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn track_ids_for_albums_returns_member_tracks() {
        // Drives dislike-album → exclude-its-tracks. Two tracks share
        // album al1; a third on al2 must not leak in.
        let store = MetadataStore::new(test_pool().await);
        let mut t1 = sample_metadata("t1");
        t1.album_id = Some("al1".into());
        let mut t2 = sample_metadata("t2");
        t2.album_id = Some("al1".into());
        let mut t3 = sample_metadata("t3");
        t3.album_id = Some("al2".into());
        store.upsert(&t1).await.unwrap();
        store.upsert(&t2).await.unwrap();
        store.upsert(&t3).await.unwrap();

        let got = store
            .track_ids_for_albums(&["al1".to_string()])
            .await
            .unwrap();
        assert_eq!(got.len(), 2);
        assert!(got.contains(&TrackId::from("t1")));
        assert!(got.contains(&TrackId::from("t2")));
        assert!(!got.contains(&TrackId::from("t3")));
    }

    #[tokio::test]
    async fn track_ids_for_artists_returns_member_tracks() {
        let store = MetadataStore::new(test_pool().await);
        let mut t1 = sample_metadata("t1");
        t1.artist_id = Some("ar1".into());
        let mut t2 = sample_metadata("t2");
        t2.artist_id = Some("ar2".into());
        store.upsert(&t1).await.unwrap();
        store.upsert(&t2).await.unwrap();

        let got = store
            .track_ids_for_artists(&["ar1".to_string()])
            .await
            .unwrap();
        assert_eq!(got, HashSet::from([TrackId::from("t1")]));
    }

    #[tokio::test]
    async fn track_ids_for_parent_empty_input_is_empty() {
        let store = MetadataStore::new(test_pool().await);
        assert!(store.track_ids_for_albums(&[]).await.unwrap().is_empty());
        assert!(store.track_ids_for_artists(&[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn null_genre_ids_returns_only_rows_with_null_genre() {
        // Drives the genre backfill: existing rows that predate the
        // ingest fix have `genre IS NULL`, and need to be re-fetched
        // from Subsonic to populate the field. Rows that already have
        // a genre must not be returned — re-fetching them would be
        // wasted work and could clobber a manually-corrected value.
        let store = MetadataStore::new(test_pool().await);
        let mut with_genre = sample_metadata("t-rock");
        with_genre.genre = Some("Rock".into());
        let mut no_genre = sample_metadata("t-null");
        no_genre.genre = None;
        store.upsert(&with_genre).await.unwrap();
        store.upsert(&no_genre).await.unwrap();

        let got = store.null_genre_ids(100).await.unwrap();
        assert_eq!(got, vec![TrackId::from("t-null")]);
    }

    #[tokio::test]
    async fn null_genre_ids_respects_limit() {
        // Limits the per-pass walk so a misconfigured backfill on a huge
        // library doesn't return megabytes in one go.
        let store = MetadataStore::new(test_pool().await);
        for i in 0..5 {
            let mut m = sample_metadata(&format!("t{i}"));
            m.genre = None;
            store.upsert(&m).await.unwrap();
        }
        let got = store.null_genre_ids(2).await.unwrap();
        assert_eq!(got.len(), 2);
    }
}
