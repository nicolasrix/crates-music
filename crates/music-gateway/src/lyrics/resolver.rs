//! Tiered lyrics resolution: cache → Navidrome → external provider.
//!
//! ## The order, and why it is that order
//!
//! 1. **Cached, unexpired row** — including a cached miss.
//! 2. **Navidrome** (`getLyricsBySongId`). The file's own tags or an
//!    `.lrc` sidecar. These win over anything external because they are
//!    ground truth *for that file*: someone who re-timed a live version or
//!    corrected a transcription meant it, and an external database has no
//!    way to know about that edit.
//! 3. **Provider, exact** — artist + title + album + duration.
//! 4. **Provider, album dropped** — tag albums drift ("Deluxe Edition",
//!    "Remastered"), and an album mismatch is weak evidence of a *song*
//!    mismatch. Duration is still supplied, so this stays tight.
//! 5. **Provider, fuzzy search** — accepted only when the candidate's
//!    duration is within tolerance of ours. Without that guard this tier
//!    happily returns a live version's words for the studio cut.
//! 6. **Nothing** — write the negative-cache row.
//!
//! ## The invariant that matters
//!
//! **A failure is never cached.** "The provider says there is no entry"
//! (a 404) is knowledge worth storing for a week; "we could not reach the
//! provider" is not knowledge at all, and storing it as a miss would turn
//! a thirty-second outage into a week of blank lyrics panels for every
//! track played during it. Tiers therefore track errors separately from
//! absences, and a run that saw only errors returns the stale row if
//! there is one and [`ResolveError::ProviderUnavailable`] otherwise.
//!
//! ## Single-flight
//!
//! Starting a track can easily issue two concurrent resolutions — the
//! player opening the panel and the queue prefetching the same track.
//! Each `track_id` gets its own async mutex; the loser wakes up, re-reads
//! the cache, and finds the winner's row instead of duplicating the
//! upstream call. A small semaphore bounds total outbound concurrency on
//! top of that.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use music_core::TrackId;
use music_recommend::{LyricLine, LyricsRow, LyricsSource, LyricsStore, MatchKind, MetadataStore};
use music_subsonic::{Client as SubsonicClient, Credentials, StructuredLyrics};
use tokio::sync::Semaphore;

use crate::config::{LyricsConfig, UpstreamConfig};

use super::lrc;
use super::lrclib::{LrclibClient, LrclibHit};

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("lyrics are disabled")]
    Disabled,

    #[error("lyrics store error: {0}")]
    Store(#[from] music_recommend::Error),

    /// Every path that could have answered errored out. Nothing was
    /// learned, so — critically — nothing was written.
    #[error("no lyrics source could be reached")]
    ProviderUnavailable,

    #[error("lyrics resolver setup: {0}")]
    Setup(String),
}

/// The lookup key an external provider needs. Sourced from the local
/// metadata cache, falling back to a `getSong` for a track the ingest
/// pipeline has not seen yet.
#[derive(Clone, Debug)]
struct LookupKey {
    artist: String,
    title: String,
    album: Option<String>,
    duration_seconds: Option<u32>,
}

pub struct LyricsResolver {
    store: LyricsStore,
    metadata: MetadataStore,
    subsonic: SubsonicClient,
    /// `None` when `external_lookup = false` — the zero-egress mode.
    lrclib: Option<LrclibClient>,
    hit_ttl_ms: i64,
    miss_ttl_ms: i64,
    duration_tolerance_seconds: u32,
    /// Bounds simultaneous outbound provider work.
    gate: Semaphore,
    /// Per-track resolution locks. Entries are removed once the last
    /// waiter is gone, so this does not grow with the catalog.
    inflight: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl std::fmt::Debug for LyricsResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LyricsResolver")
            .field("external", &self.lrclib.is_some())
            .field("hit_ttl_ms", &self.hit_ttl_ms)
            .field("miss_ttl_ms", &self.miss_ttl_ms)
            .finish_non_exhaustive()
    }
}

impl LyricsResolver {
    pub fn new(
        cfg: &LyricsConfig,
        upstream: &UpstreamConfig,
        store: LyricsStore,
        metadata: MetadataStore,
    ) -> Result<Self, ResolveError> {
        let creds = Credentials {
            username: upstream.username.clone(),
            password: upstream.password.clone(),
        };
        let subsonic = SubsonicClient::new(&upstream.navidrome_url, creds)
            .map_err(|e| ResolveError::Setup(format!("subsonic client: {e}")))?;

        let lrclib = if cfg.external_lookup {
            Some(
                LrclibClient::new(
                    &cfg.provider_url,
                    &cfg.user_agent,
                    Duration::from_secs(cfg.timeout_seconds),
                )
                .map_err(|e| ResolveError::Setup(format!("lyrics provider client: {e}")))?,
            )
        } else {
            None
        };

        Ok(Self {
            store,
            metadata,
            subsonic,
            lrclib,
            hit_ttl_ms: days_to_ms(cfg.hit_ttl_days),
            miss_ttl_ms: days_to_ms(cfg.miss_ttl_days),
            duration_tolerance_seconds: cfg.duration_tolerance_seconds,
            gate: Semaphore::new(cfg.max_concurrent.max(1)),
            inflight: Mutex::new(HashMap::new()),
        })
    }

    /// Resolve `track_id`, consulting the cache first unless `force`.
    ///
    /// `force` is the "these are the wrong lyrics" escape hatch: it skips
    /// the cache on the way in but still writes the result on the way out.
    #[tracing::instrument(name = "lyrics.resolve", skip(self), fields(track = %track_id, force))]
    pub async fn resolve(&self, track_id: &TrackId, force: bool) -> Result<LyricsRow, ResolveError> {
        if !force && let Some(row) = self.fresh_cached(track_id).await? {
            return Ok(row);
        }

        // Single-flight: hold this for the whole resolution so a
        // concurrent caller waits rather than duplicating the fetch.
        let lock = self.lock_for(track_id);
        let guard = lock.lock().await;

        // The winner may have filled the cache while we waited.
        if !force && let Some(row) = self.fresh_cached(track_id).await? {
            drop(guard);
            self.release_lock(track_id, &lock);
            return Ok(row);
        }

        let outcome = self.resolve_uncached(track_id).await;

        drop(guard);
        self.release_lock(track_id, &lock);
        outcome
    }

    /// The cached row for `track_id`, whether or not it is expired. Used
    /// by the read path to answer without touching the network when the
    /// caller only wants what we already know.
    pub async fn cached(&self, track_id: &TrackId) -> Result<Option<LyricsRow>, ResolveError> {
        Ok(self.store.get(track_id).await?)
    }

    /// Forget the cached answer so the next resolve starts from scratch.
    pub async fn forget(&self, track_id: &TrackId) -> Result<(), ResolveError> {
        self.store.delete(track_id).await?;
        Ok(())
    }

    async fn fresh_cached(&self, track_id: &TrackId) -> Result<Option<LyricsRow>, ResolveError> {
        let now = now_ms();
        Ok(self
            .store
            .get(track_id)
            .await?
            .filter(|row| !row.is_expired(now)))
    }

    /// The tiered lookup proper. Assumes the single-flight lock is held.
    async fn resolve_uncached(&self, track_id: &TrackId) -> Result<LyricsRow, ResolveError> {
        let stale = self.store.get(track_id).await?;
        let _permit = self.gate.acquire().await;
        let now = now_ms();
        // Tracks whether *any* tier failed to run, as opposed to running
        // and finding nothing. Decides cacheability at the end.
        let mut saw_error = false;

        // Tier 2 — the file's own tags win outright.
        match self.subsonic.get_lyrics_by_song_id(track_id).await {
            Ok(blocks) => {
                if let Some(row) = row_from_navidrome(track_id, &blocks, now, self.hit_ttl_ms) {
                    self.store.upsert(&row).await?;
                    return Ok(row);
                }
            }
            Err(e) => {
                tracing::warn!(track = %track_id, error = %e, "lyrics: Navidrome lookup failed");
                saw_error = true;
            }
        }

        // Tiers 3-5 need a lookup key.
        let key = match self.lookup_key(track_id).await {
            Ok(Some(key)) => Some(key),
            Ok(None) => None,
            Err(()) => {
                saw_error = true;
                None
            }
        };

        if let (Some(client), Some(key)) = (self.lrclib.as_ref(), key.as_ref()) {
            match self.resolve_external(client, track_id, key, now).await {
                Ok(Some(row)) => {
                    self.store.upsert(&row).await?;
                    return Ok(row);
                }
                Ok(None) => {}
                Err(()) => saw_error = true,
            }
        }

        // Tier 6 — nothing found. Only cacheable if every tier actually
        // ran; otherwise this is ignorance, not absence.
        if saw_error {
            if let Some(stale) = stale {
                tracing::info!(
                    track = %track_id,
                    "lyrics: provider unreachable, serving stale cached row"
                );
                return Ok(stale);
            }
            return Err(ResolveError::ProviderUnavailable);
        }

        let miss = LyricsRow::miss(track_id.clone(), now, self.miss_ttl_ms);
        self.store.upsert(&miss).await?;
        Ok(miss)
    }

    /// Tiers 3-5 against the external provider. `Ok(None)` is a clean
    /// "nothing matched"; `Err(())` means a tier could not be evaluated.
    async fn resolve_external(
        &self,
        client: &LrclibClient,
        track_id: &TrackId,
        key: &LookupKey,
        now: i64,
    ) -> Result<Option<LyricsRow>, ()> {
        let mut saw_error = false;

        // Tier 3 — exact. Only attempted when we actually have an album;
        // otherwise it is identical to tier 4 and just doubles the calls.
        if key.album.is_some() {
            match client
                .get(&key.artist, &key.title, key.album.as_deref(), key.duration_seconds)
                .await
            {
                Ok(Some(hit)) if hit.has_content() => {
                    return Ok(Some(row_from_lrclib(
                        track_id,
                        &hit,
                        MatchKind::Exact,
                        now,
                        self.hit_ttl_ms,
                    )));
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(track = %track_id, error = %e, "lyrics: provider exact lookup failed");
                    saw_error = true;
                }
            }
        }

        // Tier 4 — album dropped, duration retained.
        match client
            .get(&key.artist, &key.title, None, key.duration_seconds)
            .await
        {
            Ok(Some(hit)) if hit.has_content() => {
                let kind = if key.album.is_some() {
                    MatchKind::NoAlbum
                } else {
                    // No album to drop, so this *was* the exact lookup.
                    MatchKind::Exact
                };
                return Ok(Some(row_from_lrclib(track_id, &hit, kind, now, self.hit_ttl_ms)));
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(track = %track_id, error = %e, "lyrics: provider lookup failed");
                saw_error = true;
            }
        }

        // Tier 5 — fuzzy, duration-guarded. Skipped entirely when we do
        // not know our own duration: with no guard to apply, the tier is
        // a coin flip on which edition's words we return.
        if let Some(ours) = key.duration_seconds {
            match client.search(&key.artist, &key.title).await {
                Ok(candidates) => {
                    if let Some(hit) =
                        pick_candidate(&candidates, ours, self.duration_tolerance_seconds)
                    {
                        return Ok(Some(row_from_lrclib(
                            track_id,
                            hit,
                            MatchKind::Search,
                            now,
                            self.hit_ttl_ms,
                        )));
                    }
                }
                Err(e) => {
                    tracing::warn!(track = %track_id, error = %e, "lyrics: provider search failed");
                    saw_error = true;
                }
            }
        }

        if saw_error { Err(()) } else { Ok(None) }
    }

    /// Artist/title/album/duration for `track_id`. Prefers the local
    /// metadata cache (free, already populated by ingest) and falls back
    /// to a `getSong` for a track ingest has not reached yet.
    ///
    /// `Err(())` distinguishes "upstream call failed" from `Ok(None)`
    /// ("the track genuinely has no usable metadata"), because only the
    /// latter is safe to cache as a miss.
    async fn lookup_key(&self, track_id: &TrackId) -> Result<Option<LookupKey>, ()> {
        match self.metadata.get(track_id).await {
            Ok(Some(meta)) => {
                return Ok(Some(LookupKey {
                    artist: meta.artist,
                    title: meta.title,
                    album: meta.album.filter(|a| !a.trim().is_empty()),
                    duration_seconds: meta.duration_seconds,
                }));
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(track = %track_id, error = %e, "lyrics: metadata read failed");
                return Err(());
            }
        }

        match self.subsonic.get_song(track_id).await {
            Ok(track) => {
                let artist = track.artist_name.unwrap_or_default();
                if artist.trim().is_empty() || track.title.trim().is_empty() {
                    // Nothing to search with. A real (cacheable) absence.
                    return Ok(None);
                }
                Ok(Some(LookupKey {
                    artist,
                    title: track.title,
                    album: track.album_name.filter(|a| !a.trim().is_empty()),
                    duration_seconds: track.duration_seconds,
                }))
            }
            Err(e) => {
                tracing::warn!(track = %track_id, error = %e, "lyrics: getSong failed");
                Err(())
            }
        }
    }

    fn lock_for(&self, track_id: &TrackId) -> Arc<tokio::sync::Mutex<()>> {
        let mut map = self.inflight.lock().unwrap_or_else(PoisonError::into_inner);
        map.entry(track_id.as_str().to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Drop the per-track lock once nobody is waiting on it. Two strong
    /// references — the map's and ours — means no waiters; anything more
    /// and a queued caller still needs it.
    fn release_lock(&self, track_id: &TrackId, lock: &Arc<tokio::sync::Mutex<()>>) {
        let mut map = self.inflight.lock().unwrap_or_else(PoisonError::into_inner);
        if Arc::strong_count(lock) <= 2 {
            map.remove(track_id.as_str());
        }
    }
}

/// Pick the best block from a Navidrome response and turn it into a row.
/// A synced block always wins; an unsynced one is the consolation prize.
fn row_from_navidrome(
    track_id: &TrackId,
    blocks: &[StructuredLyrics],
    now: i64,
    ttl_ms: i64,
) -> Option<LyricsRow> {
    let synced = blocks
        .iter()
        .find(|b| b.synced && b.lines.iter().any(|l| l.start_ms.is_some()));
    let block = synced.or_else(|| {
        blocks
            .iter()
            .find(|b| b.lines.iter().any(|l| !l.text.trim().is_empty()))
    })?;

    let lines: Vec<LyricLine> = block
        .lines
        .iter()
        .filter_map(|l| {
            l.start_ms.map(|start_ms| LyricLine {
                start_ms: start_ms.max(0),
                text: l.text.trim().to_string(),
            })
        })
        .collect();
    let plain: Vec<&str> = block
        .lines
        .iter()
        .map(|l| l.text.trim())
        .filter(|t| !t.is_empty())
        .collect();
    if lines.is_empty() && plain.is_empty() {
        return None;
    }

    Some(LyricsRow {
        track_id: track_id.clone(),
        source: LyricsSource::Navidrome,
        match_kind: None,
        synced: !lines.is_empty(),
        instrumental: false,
        plain_text: (!plain.is_empty()).then(|| plain.join("\n")),
        lines,
        provider_id: None,
        fetched_at: now,
        expires_at: now.saturating_add(ttl_ms),
    })
}

fn row_from_lrclib(
    track_id: &TrackId,
    hit: &LrclibHit,
    match_kind: MatchKind,
    now: i64,
    ttl_ms: i64,
) -> LyricsRow {
    let synced_text = hit.synced_lyrics.as_deref().unwrap_or("");
    let lines = lrc::parse_lrc(synced_text);
    // Prefer the provider's own plain body; derive one from the LRC only
    // when it is missing, so a client that wants static text never has to
    // strip timestamps itself.
    let plain_text = hit
        .plain_lyrics
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            let derived = lrc::to_plain(synced_text);
            (!derived.is_empty()).then_some(derived)
        });

    LyricsRow {
        track_id: track_id.clone(),
        source: LyricsSource::Lrclib,
        match_kind: Some(match_kind),
        synced: !lines.is_empty(),
        instrumental: hit.instrumental,
        plain_text,
        lines,
        provider_id: Some(hit.id.to_string()),
        fetched_at: now,
        expires_at: now.saturating_add(ttl_ms),
    }
}

/// Best fuzzy candidate within `tolerance` seconds of our own duration,
/// preferring a synced entry over a plain one — a plain hit would
/// silently cost the line highlighting the feature exists for.
///
/// A candidate with no stated duration is rejected outright: the guard is
/// the only thing standing between "fuzzy search" and "some other
/// edition's words", so an unguardable candidate is not worth taking.
fn pick_candidate(candidates: &[LrclibHit], ours: u32, tolerance: u32) -> Option<&LrclibHit> {
    let within = |hit: &LrclibHit| {
        hit.duration_seconds.is_some_and(|d| {
            (d.round().max(0.0) - f64::from(ours)).abs() <= f64::from(tolerance)
        })
    };
    candidates
        .iter()
        .find(|h| within(h) && has_synced(h))
        .or_else(|| candidates.iter().find(|h| within(h) && h.has_content()))
}

fn has_synced(hit: &LrclibHit) -> bool {
    hit.synced_lyrics.as_deref().is_some_and(lrc::is_synced)
}

fn days_to_ms(days: u32) -> i64 {
    i64::from(days) * 24 * 60 * 60 * 1000
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_subsonic::SubsonicLyricLine;

    fn track() -> TrackId {
        TrackId::from("tr-1".to_string())
    }

    fn hit(id: i64, synced: Option<&str>, plain: Option<&str>, duration: Option<f64>) -> LrclibHit {
        LrclibHit {
            id,
            track_name: "T".into(),
            artist_name: "A".into(),
            album_name: None,
            duration_seconds: duration,
            instrumental: false,
            plain_lyrics: plain.map(ToString::to_string),
            synced_lyrics: synced.map(ToString::to_string),
        }
    }

    #[test]
    fn navidrome_prefers_the_synced_block() {
        let blocks = vec![
            StructuredLyrics {
                display_artist: None,
                display_title: None,
                lang: Some("eng".into()),
                offset_ms: 0,
                synced: false,
                lines: vec![SubsonicLyricLine { start_ms: None, text: "plain".into() }],
            },
            StructuredLyrics {
                display_artist: None,
                display_title: None,
                lang: Some("eng".into()),
                offset_ms: 0,
                synced: true,
                lines: vec![SubsonicLyricLine { start_ms: Some(1000), text: "timed".into() }],
            },
        ];
        let row = row_from_navidrome(&track(), &blocks, 0, 100).unwrap();
        assert!(row.synced);
        assert_eq!(row.lines[0].text, "timed");
        assert_eq!(row.source, LyricsSource::Navidrome);
    }

    #[test]
    fn navidrome_falls_back_to_unsynced_text() {
        let blocks = vec![StructuredLyrics {
            display_artist: None,
            display_title: None,
            lang: None,
            offset_ms: 0,
            synced: false,
            lines: vec![
                SubsonicLyricLine { start_ms: None, text: "one".into() },
                SubsonicLyricLine { start_ms: None, text: "two".into() },
            ],
        }];
        let row = row_from_navidrome(&track(), &blocks, 0, 100).unwrap();
        assert!(!row.synced);
        assert!(row.lines.is_empty());
        assert_eq!(row.plain_text.as_deref(), Some("one\ntwo"));
    }

    #[test]
    fn navidrome_empty_block_is_not_a_row() {
        // An empty block must not become a stored "hit" — it would mask
        // the external tiers forever.
        let blocks = vec![StructuredLyrics {
            display_artist: None,
            display_title: None,
            lang: None,
            offset_ms: 0,
            synced: false,
            lines: vec![SubsonicLyricLine { start_ms: None, text: "   ".into() }],
        }];
        assert!(row_from_navidrome(&track(), &blocks, 0, 100).is_none());
        assert!(row_from_navidrome(&track(), &[], 0, 100).is_none());
    }

    #[test]
    fn lrclib_row_parses_lrc_and_keeps_provider_plain_text() {
        let h = hit(9, Some("[00:01.00]one\n[00:02.00]two"), Some("one\ntwo"), Some(180.0));
        let row = row_from_lrclib(&track(), &h, MatchKind::Exact, 0, 100);
        assert!(row.synced);
        assert_eq!(row.lines.len(), 2);
        assert_eq!(row.lines[1].start_ms, 2000);
        assert_eq!(row.plain_text.as_deref(), Some("one\ntwo"));
        assert_eq!(row.provider_id.as_deref(), Some("9"));
    }

    #[test]
    fn lrclib_row_derives_plain_text_when_provider_omits_it() {
        let h = hit(9, Some("[00:01.00]only synced"), None, Some(180.0));
        let row = row_from_lrclib(&track(), &h, MatchKind::Exact, 0, 100);
        assert_eq!(row.plain_text.as_deref(), Some("only synced"));
    }

    #[test]
    fn lrclib_plain_only_hit_is_not_synced() {
        let h = hit(9, None, Some("just words"), Some(180.0));
        let row = row_from_lrclib(&track(), &h, MatchKind::NoAlbum, 0, 100);
        assert!(!row.synced);
        assert!(row.lines.is_empty());
        assert_eq!(row.match_kind, Some(MatchKind::NoAlbum));
    }

    #[test]
    fn duration_guard_rejects_a_different_edition() {
        // A live version 40 s longer than our studio cut: the exact case
        // the guard exists for.
        let candidates = vec![hit(1, Some("[00:01.00]live"), None, Some(220.0))];
        assert!(pick_candidate(&candidates, 180, 2).is_none());
    }

    #[test]
    fn duration_guard_accepts_within_tolerance_and_prefers_synced() {
        let candidates = vec![
            hit(1, None, Some("plain, but in range"), Some(181.0)),
            hit(2, Some("[00:01.00]synced, in range"), None, Some(179.0)),
        ];
        // Both pass the guard; the synced one wins because the plain one
        // would silently cost the line highlighting.
        assert_eq!(pick_candidate(&candidates, 180, 2).unwrap().id, 2);
    }

    #[test]
    fn candidate_without_duration_is_rejected() {
        let candidates = vec![hit(1, Some("[00:01.00]x"), None, None)];
        assert!(pick_candidate(&candidates, 180, 2).is_none());
    }

    #[test]
    fn plain_candidate_is_taken_when_no_synced_one_fits() {
        let candidates = vec![
            hit(1, Some("[00:01.00]synced but wrong length"), None, Some(300.0)),
            hit(2, None, Some("plain and in range"), Some(180.0)),
        ];
        assert_eq!(pick_candidate(&candidates, 180, 2).unwrap().id, 2);
    }
}
