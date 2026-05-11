//! Background ingest worker that drains the embedding queue.
//!
//! Spawned at gateway boot when an embedder client is available. Uses a
//! `music_subsonic::Client` to range-fetch the first chunk of audio for
//! each queued track, posts to the embedder sidecar, and records the
//! result via `IngestWorker`.
//!
//! Polling pattern: when there's work, drain to empty as fast as the
//! embedder can keep up; when idle, sleep `IDLE_TICK` before polling
//! again. We don't have an in-process notification channel from the
//! enqueue endpoint, so this is straightforward polling. At single-user
//! scale (~10⁴ tracks total, dozens-per-day enqueue rate) the wasted
//! wakeups are immaterial.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use music_core::TrackId;
use music_recommend::ann::AnnIndex;
use music_recommend::embedder::EmbedderClient;
use music_recommend::ingest::{
    AudioFetcher, FetchError, IngestWorker, IngestWorkerConfig, MetadataFetcher, MetadataIngest,
};
use music_recommend::metadata::{MetadataStore, TrackMetadata, normalize_title};
use music_recommend::store::EmbeddingStore;
use music_recommend::types::ModelVersion;
use music_subsonic::{Client as SubsonicClient, Credentials};
use reqwest::header::RANGE;
use tokio::task::JoinHandle;

use crate::config::UpstreamConfig;

/// Embedding window length in seconds. CLAP picks a 10 s slice from
/// whatever audio it's handed; we ask for slightly more than that so
/// the decoder has frame-boundary slack at both ends.
const WINDOW_SECONDS: u32 = 12;

/// Bitrate cap (kbps) we ask Navidrome to transcode to. Lossy MP3 lets
/// us truncate the byte stream mid-file without the decoder losing
/// sync — unlike FLAC, where a truncated range produces "decoder lost
/// sync" because the file ends mid-frame.
const TRANSCODE_MAX_BITRATE: u32 = 192;

/// Headroom on the byte range above the theoretical window size — covers
/// ID3 tags, transcoder priming frames, and the few bytes the MP3
/// decoder needs to sync to the next frame boundary after `timeOffset`.
const CLIP_HEADROOM_BYTES: u64 = 96 * 1024;

/// How many bytes to range-fetch per track. Sized for `WINDOW_SECONDS`
/// at `TRANSCODE_MAX_BITRATE` plus headroom — small enough that
/// Navidrome's ffmpeg transcode stops almost immediately after the
/// window we asked for, large enough that the decoder gets a clean
/// `WINDOW_SECONDS`-second clip.
const MAX_CLIP_BYTES: u64 =
    (TRANSCODE_MAX_BITRATE as u64 * 1000 / 8) * WINDOW_SECONDS as u64 + CLIP_HEADROOM_BYTES;

/// Idle backoff between queue polls. Snappy enough for manual testing
/// while keeping per-tick cost trivial (one indexed lookup in SQLite).
const IDLE_TICK: Duration = Duration::from_secs(5);

/// How many concurrent ingest workers to spawn. Each worker
/// independently calls `claim_next` → fetch → embed → write. SQLite's
/// atomic UPDATE…RETURNING in `claim_next` keeps two workers from
/// claiming the same row.
///
/// 8 is a good fit for the local-network case: end-to-end per-track
/// time is ~3.5 s and dominated by Subsonic transcode + range fetch,
/// so 8 in-flight fetches hide the network latency behind the GPU
/// (which is single-digit-percent of the pipeline). Higher counts hit
/// the embedder sidecar's per-process serialization on the GPU and
/// stop helping; lower counts leave the GPU idle most of the time.
const INGEST_WORKER_COUNT: usize = 8;

/// Cadence at which the ANN persister wakes and flushes any dirty
/// state to disk. Tuned for single-user steady-state ingest:
///  - ingest rates of ~1 track/few-seconds give us ~10–60 dirty
///    upserts per tick, comfortably amortising the persist cost
///    (one usearch::save + one JSON sidecar write).
///  - on hard crash, the worst-case loss is ~30 seconds of new
///    embeddings — still durable in SQLite, recovered at next
///    boot via the safety rebuild in `boot_recommender`.
const ANN_PERSIST_INTERVAL: Duration = Duration::from_secs(30);

/// Audio fetcher backed by the upstream Navidrome via the typed
/// Subsonic client. Range-caps every request at `MAX_CLIP_BYTES`.
pub struct SubsonicAudioFetcher {
    client: SubsonicClient,
}

impl std::fmt::Debug for SubsonicAudioFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubsonicAudioFetcher")
            .finish_non_exhaustive()
    }
}

impl SubsonicAudioFetcher {
    pub fn new(upstream: &UpstreamConfig) -> Result<Self> {
        let creds = Credentials {
            username: upstream.username.clone(),
            password: upstream.password.clone(),
        };
        let client = SubsonicClient::new(&upstream.navidrome_url, creds)
            .context("constructing Subsonic client for ingest")?;
        Ok(Self { client })
    }
}

/// Pick a deterministic offset (seconds) into a track of duration
/// `duration_seconds`, so that a `window_seconds`-long clip starting at
/// the offset is centered on the track's midpoint.
///
/// Returns 0 when the track is shorter than the window — the caller
/// should just embed the whole thing in that case.
///
/// Pure function, exhaustively unit-tested below. Determinism here is
/// important: re-embedding the same track on a model bump must produce
/// the same window selection so cosine deltas reflect model changes,
/// not random window drift.
fn pick_offset_seconds(duration_seconds: u32, window_seconds: u32) -> u32 {
    if duration_seconds <= window_seconds {
        return 0;
    }
    (duration_seconds - window_seconds) / 2
}

/// Metadata fetcher backed by the upstream Navidrome via the typed
/// Subsonic client. Cheap (~5ms / call); runs once per ingest as a
/// best-effort side-channel, separately from the audio fetch.
pub struct SubsonicMetadataFetcher {
    client: SubsonicClient,
}

impl std::fmt::Debug for SubsonicMetadataFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubsonicMetadataFetcher")
            .finish_non_exhaustive()
    }
}

impl SubsonicMetadataFetcher {
    pub fn new(upstream: &UpstreamConfig) -> Result<Self> {
        let creds = Credentials {
            username: upstream.username.clone(),
            password: upstream.password.clone(),
        };
        let client = SubsonicClient::new(&upstream.navidrome_url, creds)
            .context("constructing Subsonic client for metadata fetch")?;
        Ok(Self { client })
    }
}

#[async_trait]
impl MetadataFetcher for SubsonicMetadataFetcher {
    #[tracing::instrument(name = "ingest.fetch_metadata", skip(self), fields(track = %track_id))]
    async fn fetch_metadata(&self, track_id: &TrackId) -> Result<TrackMetadata, FetchError> {
        let track = self
            .client
            .get_song(track_id)
            .await
            .map_err(|e| FetchError::Transport(format!("getSong: {e}")))?;
        // Map the music-core::Track shape onto the recommend metadata
        // row. `bpm` and `musical_key` are still None because they're
        // not on the core Track today — extending the wire layer for
        // those is a separate piece of work.
        let title_normalized = normalize_title(&track.title);
        Ok(TrackMetadata {
            track_id: track.id.clone(),
            artist_id: track.artist_id.as_ref().map(|a| a.as_str().to_string()),
            artist: track.artist_name.unwrap_or_default(),
            album_id: track.album_id.as_ref().map(|a| a.as_str().to_string()),
            album: track.album_name,
            title: track.title,
            title_normalized,
            duration_seconds: track.duration_seconds,
            genre: track.genre,
            year: track.year.map(i32::from),
            track_number: track.track_number,
            disc_number: track.disc_number,
            bpm: None,
            musical_key: None,
        })
    }
}

#[async_trait]
impl AudioFetcher for SubsonicAudioFetcher {
    #[tracing::instrument(name = "ingest.fetch_clip", skip(self), fields(track = %track_id))]
    async fn fetch_clip(&self, track_id: &TrackId) -> Result<Bytes, FetchError> {
        // Look up duration so we can pick a `timeOffset` centered on
        // the track. If `getSong` fails (transient transport, missing
        // track) we fall back to offset=0 — the worst case is that we
        // embed the first `WINDOW_SECONDS` of the track, which is what
        // we'd do for a short track anyway.
        let duration_seconds = match self.client.get_song(track_id).await {
            Ok(track) => track.duration_seconds.unwrap_or(0),
            Err(e) => {
                tracing::debug!(
                    track = %track_id,
                    error = %e,
                    "ingest: getSong failed; using offset=0"
                );
                0
            }
        };
        let offset = pick_offset_seconds(duration_seconds, WINDOW_SECONDS);

        let mut url = self
            .client
            .stream_url(track_id)
            .map_err(|e| FetchError::Transport(format!("stream_url: {e}")))?;
        // Force transcode to MP3 so the byte-range cap below produces a
        // decodable prefix. FLAC sources truncated mid-file fail with
        // "decoder lost sync" inside soundfile.
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("format", "mp3")
                .append_pair("maxBitRate", &TRANSCODE_MAX_BITRATE.to_string());
            // Only attach timeOffset when non-zero. Some Subsonic
            // implementations interpret the parameter strictly and may
            // re-prime the transcoder on its presence; sending 0
            // gratuitously is just wasteful.
            if offset > 0 {
                q.append_pair("timeOffset", &offset.to_string());
            }
        }

        let range = format!("bytes=0-{}", MAX_CLIP_BYTES - 1);
        let resp = self
            .client
            .http()
            .get(url)
            .header(RANGE, range)
            .send()
            .await
            .map_err(|e| FetchError::Transport(format!("send: {e}")))?;

        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(FetchError::NotFound(track_id.clone()));
        }
        if !status.is_success() {
            return Err(FetchError::Transport(format!("upstream {status}")));
        }
        resp.bytes()
            .await
            .map_err(|e| FetchError::Transport(format!("body: {e}")))
    }
}

/// Spawn the background metadata-backfill task. Walks `done`
/// embeddings missing a metadata row, fetches their Subsonic metadata,
/// and upserts. Bounded work; exits when the missing set is empty or
/// every fetch in a batch fails. Safe to call on a fully-warm cache —
/// the first SQLite query returns empty and the task exits immediately.
pub fn spawn_metadata_backfill(
    metadata_store: MetadataStore,
    metadata_fetcher: Arc<dyn MetadataFetcher>,
    model_version: ModelVersion,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        match music_recommend::backfill_metadata(
            &metadata_store,
            metadata_fetcher.as_ref(),
            &model_version,
        )
        .await
        {
            Ok(stats) if stats.upserted > 0 || stats.fetch_failed > 0 => {
                tracing::info!(
                    upserted = stats.upserted,
                    fetch_failed = stats.fetch_failed,
                    "metadata: backfill complete"
                );
            }
            Ok(_) => {} // empty cache + empty missing set; no log spam
            Err(e) => {
                tracing::warn!(error = %e, "metadata: backfill aborted");
            }
        }
    })
}

/// Spawn the background ingest loop. Returns an empty Vec (and logs)
/// if no embedder client is available — the gateway then runs in
/// degraded mode and queued rows sit at `not_started` until a future
/// restart sees the embedder.
///
/// Spawns `INGEST_WORKER_COUNT` independent tasks, all sharing one
/// `Arc<IngestWorker>`. Each task runs the same drain-then-sleep loop;
/// SQLite-side atomicity in `claim_next` ensures rows aren't
/// double-processed.
pub fn spawn_ingest_worker(
    store: EmbeddingStore,
    ann: Arc<AnnIndex>,
    embedder: Option<EmbedderClient>,
    fetcher: Arc<dyn AudioFetcher>,
    metadata: Option<MetadataIngest>,
    model_version: &ModelVersion,
) -> Vec<JoinHandle<()>> {
    let Some(embedder) = embedder else {
        return Vec::new();
    };
    let cfg = IngestWorkerConfig {
        store,
        ann: Arc::clone(&ann),
        embedder,
        fetcher,
        model_version: model_version.clone(),
        metadata,
    };
    let worker = Arc::new(IngestWorker::new(cfg));

    tracing::info!(
        model = %model_version,
        workers = INGEST_WORKER_COUNT,
        "ingest: workers started"
    );

    let mut handles: Vec<JoinHandle<()>> = (0..INGEST_WORKER_COUNT)
        .map(|worker_id| {
            let worker = Arc::clone(&worker);
            tokio::spawn(async move {
                loop {
                    match worker.drain().await {
                        Ok(stats) if stats.embedded == 0 && stats.failed == 0 => {
                            tokio::time::sleep(IDLE_TICK).await;
                        }
                        Ok(stats) => {
                            tracing::info!(
                                worker = worker_id,
                                embedded = stats.embedded,
                                failed = stats.failed,
                                "ingest: drained queue"
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                worker = worker_id,
                                error = %e,
                                "ingest: drain failed; backing off"
                            );
                            tokio::time::sleep(IDLE_TICK).await;
                        }
                    }
                }
            })
        })
        .collect();

    handles.push(spawn_ann_persister(ann));
    handles
}

/// Background task that flushes the ANN to disk every
/// `ANN_PERSIST_INTERVAL`. Cheap when idle (a single relaxed-atomic
/// load); only takes the index read lock + writes a file when there
/// are unflushed upserts. Closes the long-standing gap where ingest
/// upserts only landed in memory and got recovered via SQLite
/// rebuild at restart — fine at 10² tracks, painful at 10⁴+.
fn spawn_ann_persister(ann: Arc<AnnIndex>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(ANN_PERSIST_INTERVAL);
        // Skip the immediate first tick — the index just rebuilt at
        // boot, no point persisting again seconds later.
        tick.tick().await;
        loop {
            tick.tick().await;
            match ann.persist_if_dirty() {
                Ok(true) => {
                    tracing::debug!("ann: persisted dirty index to disk");
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::error!(error = %e, "ann: persist failed; will retry next tick");
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_offset_returns_zero_when_track_shorter_than_window() {
        // 8 s track, 12 s window — we just embed the whole thing.
        assert_eq!(pick_offset_seconds(8, WINDOW_SECONDS), 0);
        assert_eq!(pick_offset_seconds(0, WINDOW_SECONDS), 0);
        // Boundary: track exactly the length of the window.
        assert_eq!(pick_offset_seconds(WINDOW_SECONDS, WINDOW_SECONDS), 0);
    }

    #[test]
    fn pick_offset_centers_window_on_track_midpoint() {
        // 60 s track, 12 s window: offset = (60 - 12) / 2 = 24
        // → window covers seconds 24..36, midpoint at 30 ✓
        assert_eq!(pick_offset_seconds(60, 12), 24);
        // 1042 s track, 12 s window: offset = (1042 - 12) / 2 = 515
        // → window covers seconds 515..527, midpoint 521 ≈ 1042/2 ✓
        assert_eq!(pick_offset_seconds(1042, 12), 515);
    }

    #[test]
    fn pick_offset_is_deterministic() {
        // Same input → same output, on every call. Important for
        // re-embedding stability.
        for _ in 0..100 {
            assert_eq!(pick_offset_seconds(180, WINDOW_SECONDS), 84);
        }
    }

    #[test]
    fn max_clip_bytes_matches_window_size() {
        // 12 s × 192 kbps / 8 + 96 KiB ≈ 384 KiB. The exact number isn't
        // load-bearing, but if the constant ever drifts above ~512 KiB
        // (or below ~256 KiB) the perf properties of the system change
        // materially — pin it here so the change is visible in diff.
        const _: () = assert!(MAX_CLIP_BYTES >= 256 * 1024);
        const _: () = assert!(MAX_CLIP_BYTES <= 512 * 1024);
    }
}
