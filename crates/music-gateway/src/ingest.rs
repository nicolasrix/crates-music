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
use tracing::Instrument;

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

/// Minimum seconds of decodable audio we require to sit *after* a
/// leading ID3 tag before we accept a clip.
///
/// The embedder's floor is 1 s: `Clamp3Embedder` slices the waveform
/// into `SLIDING_WINDOW_SEC`-long chunks and drops a trailing chunk
/// shorter than one second, so a clip under 1 s yields no chunks at all
/// and raises "audio too short". We use double that, deliberately: the
/// point is to retry only clips that are near-certain to fail, and to
/// leave anything that decodes today untouched. Real observed values
/// straddle this threshold with room to spare — a working clip in the
/// library carries ~4.8 s past its tag, while the broken ones carry
/// 0–1.1 s.
const MIN_AUDIO_SECONDS_AFTER_TAG: u32 = 2;

/// `maxBitRate` to request when we can't read the source bitrate and
/// have to force a transcode blind. Low enough to be under any MP3 a
/// music library realistically holds (the format's floor is 32 kbps),
/// high enough not to mangle the audio we hand the model. Only reached
/// when `getSong` failed, which also costs us the centred `timeOffset`.
const FALLBACK_FORCED_BITRATE: u32 = 96;

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

/// Append the right query params to a `/rest/stream` URL for an ingest
/// clip fetch.
///
/// We **always** transcode to MP3 at `TRANSCODE_MAX_BITRATE`, even when
/// the source is already MP3. The tempting "serve raw MP3 byte ranges"
/// fast path is a correctness trap: a VBR MP3 file carries a Xing/Info
/// header whose frame count describes the *whole* track. Range-capping
/// that file to `MAX_CLIP_BYTES` leaves the bogus full-length header in
/// place; the decoder trusts it, finds almost no frames in the
/// truncated data, and yields a ~0.1 s clip — below the embedder's 1 s
/// floor, so the embed fails with "audio too short". This silently broke
/// ingest for every VBR-header MP3 in the library (~270 tracks).
///
/// A transcode side-steps it: Navidrome streams a *fresh* MP3 with no
/// backpatched full-length header, so a Range-capped read decodes to a
/// clean ~`WINDOW_SECONDS` clip. Empirically Navidrome honours the byte
/// Range for MP3→MP3 (seekable same-container source), so the clip
/// stays bounded at `MAX_CLIP_BYTES`. For other containers (FLAC, etc.)
/// it ignores Range and streams the whole transcode — `fetch_clip` then
/// reads the full body, as it has all along; that path was never the
/// one that broke.
/// Caveat that motivates `clip_needs_transcode_retry`: "always
/// transcode" describes what we *ask for*, not what we get. Navidrome
/// only re-encodes when the request actually constrains the source, so
/// asking for `mp3@192` from a file that is already `mp3@<=192` is a
/// no-op and it streams the original bytes with our Range applied. When
/// such a file opens with a large embedded cover art, the ID3v2 tag can
/// exceed `MAX_CLIP_BYTES` outright and the "clip" we get back is pure
/// metadata — ffmpeg then fails with "Failed to find two consecutive
/// MPEG audio frames". Observed on three albums whose tags run
/// 363–677 KB against a 386 KB window.
fn apply_clip_query_params(url: &mut url::Url, offset: u32, max_bitrate: u32) {
    let mut q = url.query_pairs_mut();
    q.append_pair("format", "mp3")
        .append_pair("maxBitRate", &max_bitrate.to_string());
    // Only attach timeOffset when non-zero. Some Subsonic
    // implementations interpret the parameter strictly and may
    // re-prime the transcoder on its presence; sending 0 gratuitously
    // is just wasteful.
    if offset > 0 {
        q.append_pair("timeOffset", &offset.to_string());
    }
}

/// Total byte length of a leading ID3v2 tag, header included, or `None`
/// when the buffer doesn't start with one.
///
/// The declared size is a *syncsafe* integer: four bytes each carrying
/// seven significant bits, so that a tag length can never contain a
/// `0xFF` byte a decoder might mistake for a frame sync. A footer, when
/// the flags advertise one, adds a further ten bytes that the declared
/// size excludes.
fn id3_tag_len(bytes: &[u8]) -> Option<usize> {
    // 10-byte header: "ID3" + version(2) + flags(1) + size(4).
    let header = bytes.get(..10)?;
    if &header[..3] != b"ID3" {
        return None;
    }
    let size = header.get(6..10)?;
    // Reject a malformed size rather than silently decoding it wrong —
    // a set high bit means this isn't a syncsafe integer at all.
    if size.iter().any(|b| b & 0x80 != 0) {
        return None;
    }
    let declared = size
        .iter()
        .fold(0usize, |acc, b| (acc << 7) | (*b as usize & 0x7f));
    let footer = if header[5] & 0x10 != 0 { 10 } else { 0 };
    Some(10 + declared + footer)
}

/// Whether a fetched clip is so dominated by its leading ID3 tag that
/// the embed is near-certain to fail, and is worth one re-fetch that
/// forces Navidrome to actually transcode.
///
/// Returns false for the overwhelming majority of clips: a transcoded
/// stream carries no cover art, so there's no tag to trip on, and a
/// passthrough file with ordinary tags leaves plenty of audio inside
/// the window.
fn clip_needs_transcode_retry(clip: &[u8], bit_rate_kbps: Option<u32>) -> bool {
    let Some(tag_len) = id3_tag_len(clip) else {
        return false;
    };
    let audio_bytes = clip.len().saturating_sub(tag_len);
    // Unknown bitrate: fall back to the highest rate we ever request,
    // which yields the *largest* byte requirement and so errs toward
    // retrying. A needless retry costs one request; a missed one costs
    // a permanently unembeddable track.
    let kbps = bit_rate_kbps.unwrap_or(TRANSCODE_MAX_BITRATE).max(1);
    let required = (kbps as usize * 1000 / 8) * MIN_AUDIO_SECONDS_AFTER_TAG as usize;
    audio_bytes < required
}

/// `maxBitRate` that forces Navidrome to re-encode a source it would
/// otherwise pass through verbatim.
///
/// Navidrome transcodes only when the request constrains the source, so
/// the request has to land *strictly below* the source bitrate. One
/// kbps under is enough, and keeps the audio we hand the model as close
/// to the original as the format allows — 191 kbps in place of 192,
/// rather than a blanket downgrade that would also change every track
/// the normal path already handles.
fn forced_transcode_bitrate(bit_rate_kbps: Option<u32>) -> u32 {
    match bit_rate_kbps {
        // Clamp to the MP3 floor so a nonsense source bitrate can't
        // produce an unencodable request.
        Some(kbps) if kbps > 32 => (kbps - 1).min(TRANSCODE_MAX_BITRATE),
        Some(_) => 32,
        None => FALLBACK_FORCED_BITRATE,
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
        //
        // Wrapped in a `fetch_clip.get_song` subspan so the
        // diagnostics breakdown can show the metadata-fetch cost
        // separately from the transcoded body read.
        // We only need the duration (to centre the `timeOffset`); the
        // source codec no longer matters since we always transcode.
        let (duration_seconds, bit_rate_kbps) = match self
            .client
            .get_song(track_id)
            .instrument(tracing::info_span!("fetch_clip.get_song"))
            .await
        {
            Ok(track) => (track.duration_seconds.unwrap_or(0), track.bit_rate_kbps),
            Err(e) => {
                tracing::debug!(
                    track = %track_id,
                    error = %e,
                    "ingest: getSong failed; using offset=0"
                );
                (0, None)
            }
        };
        let offset = pick_offset_seconds(duration_seconds, WINDOW_SECONDS);

        let clip = self
            .fetch_at_bitrate(track_id, offset, TRANSCODE_MAX_BITRATE)
            .await?;
        if !clip_needs_transcode_retry(&clip, bit_rate_kbps) {
            return Ok(clip);
        }

        // Navidrome passed the source through verbatim and its cover-art
        // tag swallowed the window. Ask again just under the source
        // bitrate, which leaves it no choice but to re-encode — the
        // stream that comes back has no embedded art at all.
        let retry_bitrate = forced_transcode_bitrate(bit_rate_kbps);
        tracing::info!(
            track = %track_id,
            clip_bytes = clip.len(),
            id3_bytes = id3_tag_len(&clip).unwrap_or(0),
            retry_bitrate,
            "ingest: clip is ID3-dominated; refetching with a forced transcode"
        );
        self.fetch_at_bitrate(track_id, offset, retry_bitrate).await
    }
}

impl SubsonicAudioFetcher {
    /// One `/rest/stream` clip fetch at an explicit `maxBitRate`.
    ///
    /// Split out so the ID3 retry can re-run the identical exchange
    /// with a different bitrate rather than duplicating the span and
    /// error mapping.
    async fn fetch_at_bitrate(
        &self,
        track_id: &TrackId,
        offset: u32,
        max_bitrate: u32,
    ) -> Result<Bytes, FetchError> {
        let mut url = self
            .client
            .stream_url(track_id)
            .map_err(|e| FetchError::Transport(format!("stream_url: {e}")))?;
        apply_clip_query_params(&mut url, offset, max_bitrate);

        let range = format!("bytes=0-{}", MAX_CLIP_BYTES - 1);
        // Split the HTTP exchange into two subspans: "stream_request"
        // is everything up to the response headers (negotiation,
        // upstream queueing); "stream_body" is the body drain — that's
        // where Navidrome's real-time transcoder paces bytes, so we
        // expect this to dominate.
        let resp = self
            .client
            .http()
            .get(url)
            .header(RANGE, range)
            .send()
            .instrument(tracing::info_span!("fetch_clip.stream_request"))
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
            .instrument(tracing::info_span!("fetch_clip.stream_body"))
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

    fn build(offset: u32) -> url::Url {
        let mut url = url::Url::parse("http://nav.test/rest/stream?id=abc").unwrap();
        apply_clip_query_params(&mut url, offset, TRANSCODE_MAX_BITRATE);
        url
    }

    /// An ID3v2.4 header declaring `payload` bytes of tag, followed by
    /// `audio` bytes of stand-in frame data.
    fn id3_clip(payload: usize, audio: usize) -> Vec<u8> {
        let mut v = vec![b'I', b'D', b'3', 4, 0, 0];
        v.extend_from_slice(&[
            ((payload >> 21) & 0x7f) as u8,
            ((payload >> 14) & 0x7f) as u8,
            ((payload >> 7) & 0x7f) as u8,
            (payload & 0x7f) as u8,
        ]);
        v.resize(10 + payload, 0);
        v.resize(10 + payload + audio, 0xff);
        v
    }

    fn params(url: &url::Url) -> std::collections::BTreeMap<String, String> {
        url.query_pairs().into_owned().collect()
    }

    #[test]
    fn always_forces_mp3_transcode_with_offset() {
        // Every source transcodes — even MP3. The old raw-MP3 fast path
        // mis-decoded VBR-header MP3s when Range-truncated (see
        // `apply_clip_query_params` docs); a transcode produces a clean
        // header-less stream that survives the cap.
        let p = params(&build(84));
        assert_eq!(p.get("format").map(String::as_str), Some("mp3"));
        assert_eq!(p.get("maxBitRate").map(String::as_str), Some("192"));
        assert_eq!(p.get("timeOffset").map(String::as_str), Some("84"));
    }

    #[test]
    fn omits_time_offset_when_zero() {
        // Transcode is still forced; only the (gratuitous) timeOffset=0
        // is dropped.
        let p = params(&build(0));
        assert_eq!(p.get("format").map(String::as_str), Some("mp3"));
        assert_eq!(p.get("maxBitRate").map(String::as_str), Some("192"));
        assert!(!p.contains_key("timeOffset"), "zero offset is the default");
    }

    #[test]
    fn id3_tag_len_decodes_a_syncsafe_size() {
        // 0x00 0x29 0x27 0x48 is the real header from the blink-182
        // track that exposed this bug: 676,808 bytes of cover art.
        let mut clip = vec![b'I', b'D', b'3', 4, 0, 0, 0x00, 0x29, 0x27, 0x48];
        clip.resize(4096, 0);
        assert_eq!(id3_tag_len(&clip), Some(10 + 676_808));
    }

    #[test]
    fn id3_tag_len_accounts_for_a_footer() {
        let mut clip = id3_clip(100, 0);
        clip[5] = 0x10; // footer-present flag
        assert_eq!(id3_tag_len(&clip), Some(10 + 100 + 10));
    }

    #[test]
    fn id3_tag_len_ignores_non_id3_and_malformed_sizes() {
        // A transcoded stream starts on a frame sync, not a tag.
        assert_eq!(id3_tag_len(&[0xff, 0xfb, 0x90, 0x00]), None);
        assert_eq!(id3_tag_len(b"ID3"), None, "truncated header");
        let mut bad = id3_clip(10, 10);
        bad[7] = 0x80; // high bit set — not a syncsafe integer
        assert_eq!(id3_tag_len(&bad), None);
    }

    #[test]
    fn transcode_retry_fires_when_the_tag_eats_the_window() {
        // blink-182: the tag alone exceeds MAX_CLIP_BYTES, so the clip
        // is pure metadata and ffmpeg finds no frames at all.
        let clip = id3_clip(MAX_CLIP_BYTES as usize, 0);
        assert!(clip_needs_transcode_retry(&clip, Some(192)));

        // The Alchemist: a sliver of audio survives (~1.1 s), still
        // under the floor we require.
        let clip = id3_clip(363_488, 22_806);
        assert!(clip_needs_transcode_retry(&clip, Some(171)));
    }

    #[test]
    fn transcode_retry_leaves_working_clips_alone() {
        // A transcoded clip has no tag to trip on.
        assert!(!clip_needs_transcode_retry(&[0xff; 4096], Some(192)));

        // Sum 41: passthrough with an ordinary tag, ~4.8 s of audio
        // past it. This embeds fine today and must keep its exact
        // bytes — a retry here would silently change its vector.
        let clip = id3_clip(271_906, 114_388);
        assert!(!clip_needs_transcode_retry(&clip, Some(192)));
    }

    #[test]
    fn transcode_retry_errs_toward_retrying_without_a_bitrate() {
        // Unknown bitrate assumes the highest rate we request, so the
        // byte requirement is the largest — a borderline clip retries
        // rather than being written off.
        let clip = id3_clip(300_000, 40_000);
        assert!(clip_needs_transcode_retry(&clip, None));
    }

    #[test]
    fn forced_bitrate_lands_just_under_the_source() {
        // One kbps under is all Navidrome needs to stop passing the
        // file through, and keeps the audio near-identical.
        assert_eq!(forced_transcode_bitrate(Some(192)), 191);
        assert_eq!(forced_transcode_bitrate(Some(171)), 170);
        // Never above what the normal path would have asked for.
        assert_eq!(forced_transcode_bitrate(Some(320)), TRANSCODE_MAX_BITRATE);
        // Degenerate sources clamp to the MP3 floor instead of
        // underflowing into an unencodable request.
        assert_eq!(forced_transcode_bitrate(Some(32)), 32);
        assert_eq!(forced_transcode_bitrate(Some(0)), 32);
        assert_eq!(forced_transcode_bitrate(None), FALLBACK_FORCED_BITRATE);
    }
}
