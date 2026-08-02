//! Catalog discovery — keep the embedding queue in step with Navidrome.
//!
//! Until this existed, the only way a track entered the ingest queue was
//! an explicit `POST /v1/recommend/enqueue` (in practice:
//! `scripts/enqueue_all_tracks.py`, run by hand). New music therefore
//! browsed and played fine but stayed invisible to the recommender —
//! `/v1/recommend/next` would never surface it, and it never contributed
//! to a station — until someone remembered to run the script.
//!
//! This module closes that gap with a background watcher that enumerates
//! Navidrome and offers every track id to the store. The store's
//! `INSERT OR IGNORE` on `(track_id, model_version)` makes the whole
//! thing stateless: there's no "last seen" cursor to persist, skew, or
//! corrupt — we re-offer, and the DB decides what's new.
//!
//! ## Two scan tiers, for the same reason the browse cache has two TTLs
//!
//! - **Recent scan** (default every 5 min): `getAlbumList2?type=newest`
//!   for the first `recent_albums`, expanded via `getAlbum`. Two dozen
//!   cheap calls; catches a freshly-imported album within one interval.
//! - **Full sweep** (default every 24 h, plus once at boot): pages the
//!   entire song list via empty-query `search3`. Catches what `newest`
//!   structurally cannot — a track added to an album that already
//!   existed, since Navidrome's `newest` ordering keys on *album*
//!   creation, not track creation. Also self-seeds a fresh deployment,
//!   which is what the bulk-enqueue script used to be for.
//!
//! Both tiers are read-only against Navidrome and write only queue rows,
//! so a scan that fails halfway is simply retried on the next tick.
//!
//! Like `search::catalog`, this talks to Navidrome through
//! `music_subsonic::Client` rather than our own `/rest` proxy, so it
//! neither reads nor pollutes the L2 browse cache.

use std::time::Duration;

use anyhow::{Context, Result};
use music_core::TrackId;
use music_recommend::store::EmbeddingStore;
use music_recommend::types::ModelVersion;
use music_subsonic::{AlbumListType, Client as SubsonicClient, Credentials};
use tokio::task::JoinHandle;

use crate::config::{DiscoveryConfig, UpstreamConfig};

/// Page size for the full sweep's `search3` paging. Matches
/// `search::catalog` — Navidrome is happy with large pages, and fewer
/// round-trips means a shorter sweep.
const PAGE: u32 = 500;

/// Safety cap on full-sweep pages so a misbehaving upstream (one that
/// never returns a short page) can't loop forever. 400 × 500 = 200k
/// tracks, far above any household library.
const MAX_PAGES: u32 = 400;

/// What a scan did. Reported in logs and by the admin endpoint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ScanStats {
    /// Track ids seen in the catalog during this scan.
    pub seen: u64,
    /// Of those, how many had no queue row yet and were enqueued.
    pub enqueued: u64,
}

/// Everything a scan needs. Built once at boot and reused by the loop;
/// the admin endpoint builds a throwaway one per request.
pub struct CatalogWatcher {
    client: SubsonicClient,
    store: EmbeddingStore,
    model_version: ModelVersion,
    recent_albums: u32,
}

impl std::fmt::Debug for CatalogWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogWatcher")
            .field("model_version", &self.model_version)
            .field("recent_albums", &self.recent_albums)
            .finish_non_exhaustive()
    }
}

impl CatalogWatcher {
    pub fn new(
        upstream: &UpstreamConfig,
        store: EmbeddingStore,
        model_version: ModelVersion,
        recent_albums: u32,
    ) -> Result<Self> {
        let creds = Credentials {
            username: upstream.username.clone(),
            password: upstream.password.clone(),
        };
        let client = SubsonicClient::new(&upstream.navidrome_url, creds)
            .context("constructing Subsonic client for catalog discovery")?;
        Ok(Self {
            client,
            store,
            model_version,
            recent_albums,
        })
    }

    /// Fast tier: expand the newest `recent_albums` albums and enqueue
    /// any track we haven't seen.
    ///
    /// Deliberately *not* short-circuited on the first fully-known
    /// album. Navidrome's `newest` ordering is by album creation, and an
    /// album can gain tracks after it was created (a partial import
    /// completing, a disc 2 arriving later), which would leave a gap
    /// behind the stop point. Scanning all `recent_albums` costs a
    /// couple of dozen ~5 ms calls, so there is nothing to buy by
    /// stopping early.
    #[tracing::instrument(name = "discovery.scan_recent", skip(self))]
    pub async fn scan_recent(&self) -> Result<ScanStats> {
        let albums = self
            .client
            .get_album_list2(AlbumListType::Newest, Some(self.recent_albums), Some(0))
            .await
            .context("discovery: get_album_list2(newest)")?;

        let mut track_ids: Vec<TrackId> = Vec::new();
        for album in &albums {
            match self.client.get_album(&album.id).await {
                Ok(full) => track_ids.extend(full.tracks.into_iter().map(|t| t.id)),
                // One unreadable album shouldn't sink the scan; the next
                // tick retries it, and the full sweep covers it anyway.
                Err(e) => tracing::warn!(
                    album = %album.id,
                    error = %e,
                    "discovery: getAlbum failed; skipping album this scan"
                ),
            }
        }
        self.enqueue(track_ids).await
    }

    /// Slow tier: page the whole song list and enqueue everything
    /// missing. Uses empty-query `search3`, the same technique
    /// `search::catalog` and the web client use to enumerate tracks.
    #[tracing::instrument(name = "discovery.scan_full", skip(self))]
    pub async fn scan_full(&self) -> Result<ScanStats> {
        let mut track_ids: Vec<TrackId> = Vec::new();
        for page in 0..MAX_PAGES {
            let result = self
                .client
                .search3("", PAGE, page * PAGE)
                .await
                .context("discovery: search3 (tracks)")?;
            let is_last = result.tracks.len() < PAGE as usize;
            track_ids.extend(result.tracks.into_iter().map(|t| t.id));
            if is_last {
                break;
            }
        }
        self.enqueue(track_ids).await
    }

    /// Dedupe and hand the batch to the store. Dedupe matters for the
    /// full sweep (a track can be returned twice if the underlying list
    /// shifts between pages) and costs nothing for the recent scan.
    async fn enqueue(&self, track_ids: Vec<TrackId>) -> Result<ScanStats> {
        let mut unique: Vec<TrackId> = track_ids;
        unique.sort_unstable_by(|a, b| a.as_str().cmp(b.as_str()));
        unique.dedup_by(|a, b| a.as_str() == b.as_str());
        // Counted after dedupe so `seen` means "distinct tracks in the
        // catalog", not "rows the upstream happened to hand us".
        let seen = unique.len() as u64;

        let enqueued = self
            .store
            .enqueue_many(&unique, &self.model_version)
            .await
            .context("discovery: enqueue_many")?;
        Ok(ScanStats { seen, enqueued })
    }
}

/// Spawn the discovery loop. Returns `None` when discovery is disabled,
/// so the caller can log that once at boot.
///
/// Shape: one full sweep immediately (seeding a fresh install and
/// catching anything that landed while the gateway was down), then a
/// recent scan every `interval`, promoting to a full sweep whenever
/// `full_interval` has elapsed. A zero `interval` disables the loop
/// entirely — the boot sweep still runs, so `interval_seconds = 0` is a
/// usable "scan once at startup" mode.
///
/// Runs regardless of whether the embedder is reachable: queue rows are
/// durable, and the ingest worker drains them whenever the sidecar comes
/// back. Enqueueing during an embedder outage is the *point* — nothing
/// gets lost, it just waits.
pub fn spawn_catalog_watch(
    watcher: CatalogWatcher,
    cfg: &DiscoveryConfig,
) -> Option<JoinHandle<()>> {
    if !cfg.enabled {
        return None;
    }
    let interval = Duration::from_secs(cfg.interval_seconds);
    let full_interval = Duration::from_secs(cfg.full_interval_seconds);

    Some(tokio::spawn(async move {
        run_scan(&watcher, ScanKind::Full).await;
        if interval.is_zero() {
            tracing::info!("discovery: interval is 0; boot sweep only");
            return;
        }
        let mut since_full = Duration::ZERO;
        loop {
            tokio::time::sleep(interval).await;
            since_full += interval;

            let kind = if !full_interval.is_zero() && since_full >= full_interval {
                since_full = Duration::ZERO;
                ScanKind::Full
            } else {
                ScanKind::Recent
            };
            run_scan(&watcher, kind).await;
        }
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanKind {
    Recent,
    Full,
}

impl ScanKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Recent => "recent",
            Self::Full => "full",
        }
    }
}

/// Run one scan and log the outcome. Errors are logged, never
/// propagated: a transient Navidrome hiccup must not kill the loop —
/// the next tick retries, and the queue is unchanged in the meantime.
async fn run_scan(watcher: &CatalogWatcher, kind: ScanKind) {
    let result = match kind {
        ScanKind::Recent => watcher.scan_recent().await,
        ScanKind::Full => watcher.scan_full().await,
    };
    match result {
        // Only log when something actually changed. The steady state is
        // "nothing new", once every interval, forever — logging that
        // would bury the interesting lines.
        Ok(stats) if stats.enqueued > 0 => tracing::info!(
            scan = kind.as_str(),
            seen = stats.seen,
            enqueued = stats.enqueued,
            "discovery: queued new tracks for embedding"
        ),
        Ok(stats) => tracing::debug!(
            scan = kind.as_str(),
            seen = stats.seen,
            "discovery: no new tracks"
        ),
        Err(e) => tracing::warn!(
            scan = kind.as_str(),
            error = %e,
            "discovery: scan failed; retrying next tick"
        ),
    }
}
