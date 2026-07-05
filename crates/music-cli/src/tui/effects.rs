//! Effect runner: every `Effect` becomes one detached tokio task that does
//! its I/O and reports back as a `Msg` (exactly one, except audio prefetch
//! and scrobble, whose failures are deliberately silent — prefetch because
//! the resolve path retries and surfaces the error when the track actually
//! plays, scrobble because a lost play count never warrants a status line).

use std::sync::Arc;

use music_core::TrackId;
use music_sync::SyncOp;
use tokio::sync::{Mutex, mpsc::UnboundedSender};

use crate::api::{self, ApiError};
use crate::config::Config;
use music_cache::AudioCache;
use music_subsonic::Client;

use super::msg::{Effect, Msg, StationError, SyncEvent};
use super::state::{LikedEntry, Rating};

/// Shared handles the effect tasks need. Cheap to clone (all Arcs).
#[derive(Clone)]
pub(crate) struct Ctx {
    pub config: Arc<Config>,
    client: Arc<Mutex<ClientSlot>>,
    pub cache: Arc<AudioCache>,
    pub msg_tx: UnboundedSender<Msg>,
    /// Sink into the sync WS task for outbound ops; `None` in direct mode
    /// (no gateway, so no sync connection was spawned).
    sync_ops: Option<UnboundedSender<SyncOp>>,
}

impl std::fmt::Debug for Ctx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx").finish_non_exhaustive()
    }
}

/// The Subsonic client with the bearer it was built with. OAuth access
/// tokens rotate (~hourly), and the client bakes the token into its default
/// headers — so before each use we compare against the freshly-resolved
/// bearer and rebuild when it changed. That keeps a TUI session alive
/// across token rotations without any 401-retry machinery.
struct ClientSlot {
    bearer: Option<String>,
    client: Client,
}

impl Ctx {
    pub(crate) fn new(
        config: Arc<Config>,
        client: Client,
        bearer: Option<String>,
        cache: Arc<AudioCache>,
        msg_tx: UnboundedSender<Msg>,
        sync_ops: Option<UnboundedSender<SyncOp>>,
    ) -> Self {
        Self {
            config,
            client: Arc::new(Mutex::new(ClientSlot { bearer, client })),
            cache,
            msg_tx,
            sync_ops,
        }
    }

    async fn subsonic(&self) -> anyhow::Result<Client> {
        let Some(gw) = &self.config.gateway else {
            // Direct mode: credentials are static, the client never staleness.
            return Ok(self.client.lock().await.client.clone());
        };
        let bearer = crate::auth::resolve_bearer(&self.config, gw).await?;
        let mut slot = self.client.lock().await;
        if slot.bearer.as_deref() != Some(bearer.as_str()) {
            slot.client = crate::app::build_client(&self.config).await?;
            slot.bearer = Some(bearer);
        }
        Ok(slot.client.clone())
    }
}

pub(crate) fn spawn(effect: Effect, ctx: &Ctx) {
    let ctx = ctx.clone();
    tokio::spawn(async move {
        if let Some(msg) = run(effect, &ctx).await {
            let _ = ctx.msg_tx.send(msg);
        }
    });
}

#[allow(clippy::too_many_lines)] // one arm per effect; splitting hurts more
async fn run(effect: Effect, ctx: &Ctx) -> Option<Msg> {
    match effect {
        Effect::LoadAlbums {
            generation,
            kind,
            size,
        } => {
            let result = async {
                Ok::<_, anyhow::Error>(
                    ctx.subsonic()
                        .await?
                        .get_album_list2(kind, Some(size), None)
                        .await?,
                )
            }
            .await
            .map_err(|e| e.to_string());
            Some(Msg::AlbumsLoaded { generation, result })
        }
        Effect::OpenAlbum { id } => {
            let result = fetch_album(ctx, &id).await;
            Some(Msg::AlbumOpened { id, result })
        }
        Effect::EnqueueAlbum { id } => {
            let result = fetch_album(ctx, &id).await;
            Some(Msg::AlbumTracksForEnqueue { result })
        }
        Effect::Search { generation, query } => {
            let result = if ctx.config.gateway.is_some() {
                api::v1_search(&ctx.config, &query, 20)
                    .await
                    .map_err(|e| e.to_string())
            } else {
                // Direct mode has no gateway fuzzy index — plain search3.
                async {
                    Ok::<_, anyhow::Error>(ctx.subsonic().await?.search3(&query, 20, 0).await?)
                }
                .await
                .map_err(|e| e.to_string())
            };
            Some(Msg::SearchDone { generation, result })
        }
        Effect::Station {
            generation,
            prompt,
            n,
        } => {
            let result = match api::station(&ctx.config, &prompt, n).await {
                Ok(list) => resolve_list(ctx, &list.track_ids).await,
                Err(e) => Err(station_error(e)),
            };
            Some(Msg::StationDone { generation, result })
        }
        Effect::RecommendNext { seed, n } => {
            let result = match api::recommend_next(&ctx.config, &seed, n).await {
                Ok(list) => resolve_list(ctx, &list.track_ids).await,
                Err(e) => Err(station_error(e)),
            };
            Some(Msg::RecommendDone { result })
        }
        Effect::LoadLiked => {
            let result = load_liked(ctx).await;
            Some(Msg::LikedLoaded { result })
        }
        Effect::SetRating {
            kind,
            id,
            verdict,
            previous,
        } => {
            let result = api::set_rating(&ctx.config, kind, &id, verdict.map(Rating::wire))
                .await
                .map_err(|e| e.to_string());
            Some(Msg::RatingSet {
                id,
                previous,
                result,
            })
        }
        Effect::ResolveAudio {
            queue_index,
            track_id,
        } => {
            let fetched = async {
                let client = ctx.subsonic().await?;
                crate::app::fetch_track_bytes(&client, &ctx.cache, &TrackId::from(track_id.clone()))
                    .await
            }
            .await;
            Some(match fetched {
                Ok(bytes) => Msg::AudioReady {
                    queue_index,
                    track_id,
                    bytes,
                },
                Err(e) => Msg::AudioFailed {
                    queue_index,
                    track_id,
                    error: e.to_string(),
                },
            })
        }
        Effect::PrefetchAudio { track_id } => {
            let fetched = async {
                let client = ctx.subsonic().await?;
                crate::app::fetch_track_bytes(&client, &ctx.cache, &TrackId::from(track_id.clone()))
                    .await
            }
            .await;
            match fetched {
                Ok(bytes) => Some(Msg::PrefetchReady { track_id, bytes }),
                Err(e) => {
                    tracing::debug!(track = track_id, error = %e, "prefetch failed");
                    None
                }
            }
        }
        Effect::Scrobble {
            track_id,
            submission,
        } => {
            // Deliberately silent on failure (see the Effect doc): a play
            // count is not worth a status line, and the next track retries
            // the connection anyway.
            let sent = async {
                let client = ctx.subsonic().await?;
                client
                    .scrobble(&TrackId::from(track_id.clone()), submission)
                    .await?;
                Ok::<_, anyhow::Error>(())
            }
            .await;
            if let Err(e) = sent {
                tracing::debug!(track = track_id, submission, error = %e, "scrobble failed");
            }
            None
        }
        Effect::FlushEvents { events } => {
            let outgoing: Vec<_> = events.iter().map(super::signal::PendingEvent::to_outgoing).collect();
            let result = api::post_events(&ctx.config, &outgoing)
                .await
                .map_err(|e| e.to_string());
            Some(Msg::EventsFlushed { events, result })
        }
        Effect::SyncSubmit { op } => {
            // Hand the op to the WS task. If the channel is gone (task
            // exited / direct mode), the op is lost — the reducer already
            // runs the queue locally when offline, and every outcome
            // otherwise comes back as a `Sync` frame, so there's no Msg
            // to emit here (the third silent-effect exception).
            if let Some(tx) = &ctx.sync_ops {
                if tx.send(op).is_err() {
                    tracing::debug!("sync op dropped — WS task gone");
                }
            } else {
                tracing::debug!("sync op with no WS task (direct mode)");
            }
            None
        }
        Effect::SyncResync => {
            let ev = match api::sync_snapshot(&ctx.config).await {
                Ok(state) => SyncEvent::Frame(music_sync::ServerMessage::Snapshot { state }),
                Err(e) => SyncEvent::Down {
                    reason: format!("resync failed: {e}"),
                },
            };
            Some(Msg::Sync(ev))
        }
        Effect::LoadWhoami => {
            let result = api::whoami(&ctx.config).await.map_err(|e| e.to_string());
            Some(Msg::WhoamiLoaded { result })
        }
        Effect::HydrateTracks { ids } => {
            let result = async {
                let client = ctx.subsonic().await?;
                let track_ids: Vec<TrackId> =
                    ids.iter().map(|id| TrackId::from(id.clone())).collect();
                let (tracks, _failed) = api::resolve_tracks(&client, &track_ids).await;
                Ok::<_, anyhow::Error>(tracks)
            }
            .await
            .map_err(|e| e.to_string());
            Some(Msg::TracksHydrated { ids, result })
        }
    }
}

async fn fetch_album(
    ctx: &Ctx,
    id: &music_core::AlbumId,
) -> Result<music_subsonic::AlbumWithSongs, String> {
    async {
        Ok::<_, anyhow::Error>(ctx.subsonic().await?.get_album(id).await?)
    }
    .await
    .map_err(|e| e.to_string())
}

fn station_error(e: ApiError) -> StationError {
    match e {
        ApiError::RecommenderUnavailable => StationError::Unavailable,
        // Reads are any-authenticated, so a 403 here isn't expected; surface
        // it as a plain error rather than inventing a station-specific case.
        ApiError::Forbidden => StationError::Other(e.to_string()),
        ApiError::Http(e) => StationError::Other(e.to_string()),
    }
}

/// Resolve recommender ids to tracks; unresolvable ids are dropped (the
/// ranking survives, minus holes — same policy as the classic commands,
/// which list failures on stderr the TUI doesn't have).
async fn resolve_list(
    ctx: &Ctx,
    ids: &[TrackId],
) -> Result<Vec<music_core::Track>, StationError> {
    let client = ctx
        .subsonic()
        .await
        .map_err(|e| StationError::Other(e.to_string()))?;
    let (tracks, failed) = api::resolve_tracks(&client, ids).await;
    if !failed.is_empty() {
        tracing::debug!(failed = failed.len(), "station ids failed to resolve");
    }
    Ok(tracks)
}

async fn load_liked(ctx: &Ctx) -> Result<Vec<LikedEntry>, String> {
    let ratings = api::fetch_ratings(&ctx.config)
        .await
        .map_err(|e| e.to_string())?;
    let client = ctx.subsonic().await.map_err(|e| e.to_string())?;

    let mut entries = Vec::with_capacity(ratings.len());
    // Resolve track titles concurrently, keeping server order.
    let track_ids: Vec<TrackId> = ratings
        .iter()
        .filter(|r| r.kind == "track")
        .map(|r| TrackId::from(r.id.clone()))
        .collect();
    let (tracks, _) = api::resolve_tracks(&client, &track_ids).await;
    let by_id: std::collections::HashMap<&str, &music_core::Track> =
        tracks.iter().map(|t| (t.id.as_str(), t)).collect();

    for item in &ratings {
        let rating = match item.rating.as_deref() {
            Some("like") => Rating::Like,
            Some("dislike") => Rating::Dislike,
            _ => continue,
        };
        entries.push(LikedEntry {
            kind: item.kind.clone(),
            id: item.id.clone(),
            rating,
            track: by_id.get(item.id.as_str()).map(|t| (*t).clone()),
        });
    }
    // Likes first, then dislikes — matches the classic `liked` sections.
    entries.sort_by_key(|e| matches!(e.rating, Rating::Dislike));
    Ok(entries)
}
