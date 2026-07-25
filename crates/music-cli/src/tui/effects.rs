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

use super::msg::{DiagData, Effect, Msg, StationError, SyncEvent};
use super::state::{
    ArtistDetailState, ArtistRow, DiagTab, DiagWindow, FeedbackVote, LikedEntry,
    PlaylistDetailState, Rating, SimilarEntry, SimilarKind, TracingData,
};

mod downloads;
mod refill;
mod settings;

pub(crate) use settings::LiveSettings;

/// Shared handles the effect tasks need. Cheap to clone (all Arcs).
#[derive(Clone)]
pub(crate) struct Ctx {
    pub config: Arc<Config>,
    client: Arc<Mutex<ClientSlot>>,
    pub cache: Arc<AudioCache>,
    pub msg_tx: UnboundedSender<Msg>,
    /// Live, runtime-editable settings — see [`LiveSettings`]. A `std::Mutex`
    /// (not tokio's) because it's only ever held for a lock-copy-unlock with
    /// no `.await` in between.
    settings: Arc<std::sync::Mutex<LiveSettings>>,
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
        settings: LiveSettings,
        msg_tx: UnboundedSender<Msg>,
        sync_ops: Option<UnboundedSender<SyncOp>>,
    ) -> Self {
        Self {
            config,
            client: Arc::new(Mutex::new(ClientSlot { bearer, client })),
            cache,
            settings: Arc::new(std::sync::Mutex::new(settings)),
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
        Effect::LoadArtists { generation } => {
            let result = async { Ok::<_, anyhow::Error>(ctx.subsonic().await?.get_artists().await?) }
                .await
                .map_err(|e| e.to_string());
            Some(Msg::ArtistsLoaded { generation, result })
        }
        Effect::LoadSongs { generation } => {
            // Empty search3 matches the whole library; first page only.
            let result = async {
                Ok::<_, anyhow::Error>(ctx.subsonic().await?.search3("", SONGS_PAGE, 0).await?.tracks)
            }
            .await
            .map_err(|e| e.to_string());
            Some(Msg::SongsLoaded { generation, result })
        }
        Effect::OpenArtist { id, name } => {
            let result = open_artist(ctx, &id, &name).await;
            Some(Msg::ArtistOpened { id, result })
        }
        Effect::LoadAlbumSimilar {
            album_id,
            artist_id,
            seed_track_ids,
        } => {
            let result = album_similar(ctx, &album_id, artist_id.as_deref(), &seed_track_ids).await;
            Some(Msg::AlbumSimilarLoaded { album_id, result })
        }
        Effect::AlbumStation { candidate_seeds } => {
            // An album station replaces the queue, so no queue_context dedup.
            let result =
                match api::recommend_from_any(&ctx.config, &candidate_seeds, ALBUM_STATION_N, &[], None)
                    .await
                {
                    Ok(list) => resolve_list(ctx, &list.track_ids).await,
                    Err(e) => Err(station_error(e)),
                };
            Some(Msg::AlbumStationDone { result })
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
                crate::app::fetch_track_bytes(
                    &client,
                    &ctx.cache,
                    &TrackId::from(track_id.clone()),
                    ctx.stream_quality(),
                )
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
                crate::app::fetch_track_bytes(
                    &client,
                    &ctx.cache,
                    &TrackId::from(track_id.clone()),
                    ctx.stream_quality(),
                )
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
        Effect::AutoplayRefill {
            queue_track_ids,
            now_playing_index,
            recommended,
            anchor_track_id,
            session_id,
            need,
            generation,
        } => {
            let result = refill::run(
                ctx,
                &queue_track_ids,
                now_playing_index,
                &recommended,
                anchor_track_id.as_deref(),
                &session_id,
                need,
            )
            .await;
            Some(Msg::AutoplayRefilled {
                generation,
                need,
                result,
            })
        }
        Effect::SubmitFeedback {
            track_id,
            vote,
            session_id,
            previous,
        } => {
            let occurred = crate::auth::store::now_ms();
            let result = api::submit_feedback(
                &ctx.config,
                &track_id,
                vote.map(FeedbackVote::wire),
                &session_id,
                occurred,
            )
            .await
            .map(|_totals| ())
            .map_err(|e| e.to_string());
            Some(Msg::FeedbackDone {
                track_id,
                previous,
                result,
            })
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

        // ── playlists ─────────────────────────────────────────────────
        Effect::LoadPlaylists { generation } => {
            let result = api::list_playlists(&ctx.config)
                .await
                .map_err(|e| e.to_string());
            Some(Msg::PlaylistsLoaded { generation, result })
        }
        Effect::OpenPlaylist { id } => {
            let result = open_playlist(ctx, &id).await;
            Some(Msg::PlaylistOpened { id, result })
        }
        Effect::PlaylistCreate { name, then_add } => {
            Some(playlist_create(ctx, &name, then_add.as_deref()).await)
        }
        Effect::PlaylistRename { id, name } => {
            let msg = match api::rename_playlist(&ctx.config, &id, &name).await {
                Ok(()) => Msg::PlaylistWriteDone {
                    note: format!("renamed to {name}"),
                    is_error: false,
                    reload_list: true,
                    reopen_id: Some(id),
                },
                Err(e) => write_error(&e),
            };
            Some(msg)
        }
        Effect::PlaylistDelete { id } => {
            let msg = match api::delete_playlist(&ctx.config, &id).await {
                Ok(()) => Msg::PlaylistWriteDone {
                    note: "deleted playlist".to_owned(),
                    is_error: false,
                    reload_list: true,
                    reopen_id: None,
                },
                // Refresh the list either way so a failed delete's playlist
                // reappears rather than lingering half-removed.
                Err(e) => Msg::PlaylistWriteDone {
                    note: format!("delete failed: {}", playlist_err(&e)),
                    is_error: true,
                    reload_list: true,
                    reopen_id: None,
                },
            };
            Some(msg)
        }
        Effect::PlaylistAddTrack { id, track_id } => {
            // reopen_id is set to the target on both arms so an add to the
            // *currently-open* playlist refreshes its detail (on success) or
            // rolls back an optimistic append (on failure). on_write_done
            // reopens only when it's still the playlist on screen, so a
            // picker-add to some other playlist doesn't hijack the view.
            let msg = match api::put_playlist_tracks(&ctx.config, &id, &[track_id], true).await {
                Ok(()) => Msg::PlaylistWriteDone {
                    note: "added to playlist".to_owned(),
                    is_error: false,
                    reload_list: true,
                    reopen_id: Some(id),
                },
                Err(e) => Msg::PlaylistWriteDone {
                    note: format!("add failed: {}", playlist_err(&e)),
                    is_error: true,
                    reload_list: false,
                    reopen_id: Some(id),
                },
            };
            Some(msg)
        }
        Effect::PlaylistSetTracks { id, track_ids } => {
            let msg = match api::put_playlist_tracks(&ctx.config, &id, &track_ids, false).await {
                Ok(()) => Msg::PlaylistWriteDone {
                    note: "playlist updated".to_owned(),
                    is_error: false,
                    reload_list: true,
                    reopen_id: None,
                },
                // Reopen to resync the optimistic edit against the server.
                Err(e) => Msg::PlaylistWriteDone {
                    note: format!("playlist edit failed: {}", playlist_err(&e)),
                    is_error: true,
                    reload_list: false,
                    reopen_id: Some(id),
                },
            };
            Some(msg)
        }
        Effect::PlaylistSuggest { playlist_id, seeds } => {
            let result = match api::suggest_from_seeds(&ctx.config, &seeds, PLAYLIST_SUGGEST_N).await
            {
                // Distinguish "not embedded yet" (an ingest gap the user can
                // wait out) from "nothing new to add" (an empty Ok, which
                // on_suggestions words differently).
                Ok(list) if list.all_seeds_unindexed => Err(StationError::Other(
                    "playlist isn't embedded yet — suggestions improve after ingest".to_owned(),
                )),
                Ok(list) => resolve_list(ctx, &list.track_ids).await,
                Err(e) => Err(station_error(e)),
            };
            Some(Msg::PlaylistSuggestionsDone {
                playlist_id,
                result,
            })
        }

        // ── downloads / offline ─────────────────────────────────────────
        Effect::LoadDownloads => {
            let (stats, pinned) = downloads::load(ctx).await;
            Some(Msg::DownloadsLoaded { stats, pinned })
        }
        Effect::PinToggle { track_id, title } => {
            Some(downloads::pin_toggle(ctx, &track_id, &title).await)
        }
        Effect::PinBulk { track_ids, label } => {
            Some(downloads::pin_bulk(ctx, &track_ids, &label).await)
        }
        Effect::WarmLiked => Some(downloads::warm_liked(ctx).await),
        Effect::EvictCache => Some(downloads::evict(ctx).await),

        // ── settings ──────────────────────────────────────────────────
        Effect::SaveSettings(payload) => Some(settings::save(ctx, payload).await),
        Effect::SignOut => Some(settings::sign_out(ctx).await),
        Effect::CacheInvalidate => Some(settings::invalidate_cache(ctx).await),

        // ── diagnostics ───────────────────────────────────────────────
        Effect::LoadDiagnostics { tab, window } => Some(diagnostics_load(ctx, tab, window).await),
        Effect::LoadLatentNeighbours { track_id } => {
            Some(diagnostics_neighbours(ctx, track_id).await)
        }
    }
}

/// Rows fetched for the tabular diagnostics inspectors.
const DIAG_TABLE_LIMIT: u32 = 100;
/// Nearest neighbours fetched for the selected latent-space point.
const LATENT_NEIGHBOURS_K: usize = 12;

/// Load one diagnostics sub-tab. Resolves the window to a `since_ms` cutoff
/// here (the reducer stays clock-free), then dispatches to the tab's fetcher.
async fn diagnostics_load(ctx: &Ctx, tab: DiagTab, window: DiagWindow) -> Msg {
    let since = window.cutoff_ms(crate::auth::store::now_ms());
    let result = load_diag_tab(ctx, tab, since).await.map_err(diag_err);
    Msg::DiagnosticsLoaded { tab, result }
}

async fn load_diag_tab(ctx: &Ctx, tab: DiagTab, since: Option<i64>) -> Result<DiagData, ApiError> {
    let cfg = &ctx.config;
    Ok(match tab {
        DiagTab::Ingest => DiagData::Ingest(api::queue_depth(cfg).await?),
        DiagTab::Recommender => {
            DiagData::Recommender(Box::new(api::recommender_panels(cfg, since).await?))
        }
        DiagTab::Listening => {
            DiagData::Listening(api::recently_played(cfg, DIAG_TABLE_LIMIT).await?)
        }
        DiagTab::Tracing => {
            // Traces + histogram share the tab's window; fetch concurrently.
            let (mut traces, histogram) = tokio::try_join!(
                api::traces(cfg, DIAG_TABLE_LIMIT as usize),
                api::histogram(cfg, since),
            )?;
            // Group by trace, ordered within each — makes the span tree read
            // top-down and keeps the nav selection stable across refreshes.
            traces.sort_by(|a, b| {
                a.trace_id
                    .cmp(&b.trace_id)
                    .then(a.start_ms.cmp(&b.start_ms))
            });
            DiagData::Tracing(TracingData { traces, histogram })
        }
        DiagTab::LatentSpace => DiagData::Latent(Box::new(api::latent_space(cfg).await?)),
        DiagTab::ClientEvents => {
            DiagData::ClientEvents(api::client_events(cfg, DIAG_TABLE_LIMIT as usize).await?)
        }
    })
}

/// Fetch the selected point's raw neighbours, then hydrate their titles (the
/// endpoint returns ids + cosine distances only — best-effort names give the
/// side list something readable).
async fn diagnostics_neighbours(ctx: &Ctx, track_id: String) -> Msg {
    let result = async {
        let mut neighbours =
            api::latent_neighbours(&ctx.config, &track_id, LATENT_NEIGHBOURS_K).await?;
        let client = ctx.subsonic().await?;
        let ids: Vec<TrackId> = neighbours
            .iter()
            .map(|n| TrackId::from(n.track_id.clone()))
            .collect();
        let (tracks, _failed) = api::resolve_tracks(&client, &ids).await;
        let by_id: std::collections::HashMap<&str, &music_core::Track> =
            tracks.iter().map(|t| (t.id.as_str(), t)).collect();
        for n in &mut neighbours {
            if let Some(t) = by_id.get(n.track_id.as_str()) {
                n.title = Some(t.title.clone());
                n.artist.clone_from(&t.artist_name);
            }
        }
        Ok::<_, ApiError>(neighbours)
    }
    .await
    .map_err(diag_err);
    Msg::LatentNeighboursLoaded {
        seed: track_id,
        result,
    }
}

/// Friendly text for a diagnostics `ApiError`. A 403 shouldn't happen (the
/// section is admin-gated in the UI), but if whoami is stale, say so plainly
/// rather than dumping an HTTP error; a 404 means "no data yet".
fn diag_err(e: ApiError) -> String {
    match e {
        ApiError::Forbidden => "diagnostics are admin-only".to_owned(),
        ApiError::RecommenderUnavailable => {
            "recommender not ready — no data for this panel yet".to_owned()
        }
        ApiError::Http(e) => e.to_string(),
    }
}

/// Suggestion count for the playlist "suggest more" (`from-seeds`) path.
const PLAYLIST_SUGGEST_N: usize = 20;
/// Full-library page size for Tracks mode.
const SONGS_PAGE: u32 = 250;
/// How many tracks an album/artist station enqueues.
const ALBUM_STATION_N: usize = 40;
/// Similar albums / artists requested for the album footer.
const SIMILAR_N: usize = 8;
/// Top songs fetched for the artist-detail pane.
const ARTIST_TOP_SONGS: u32 = 20;

/// `getArtist` + `getTopSongs` → the artist-detail pane's flat row list
/// (albums first, then top songs). Top songs are best-effort — an artist
/// with no play data still opens with just their albums.
async fn open_artist(ctx: &Ctx, id: &str, name: &str) -> Result<ArtistDetailState, String> {
    let client = ctx.subsonic().await.map_err(|e| e.to_string())?;
    let awa = client
        .get_artist(&music_core::ArtistId::from(id.to_owned()))
        .await
        .map_err(|e| e.to_string())?;
    let top = client
        .get_top_songs(name, ARTIST_TOP_SONGS)
        .await
        .unwrap_or_default();
    let albums_len = awa.albums.len();
    let mut rows: Vec<ArtistRow> = awa.albums.into_iter().map(ArtistRow::Album).collect();
    rows.extend(top.into_iter().map(ArtistRow::Song));
    Ok(ArtistDetailState {
        artist: awa.artist,
        rows,
        albums_len,
    })
}

/// The album-detail "you might like" footer: `similar_albums` +
/// `similar_artists`, hydrated to names. A degraded/warming recommender
/// yields an empty footer (not an error) — the album view still works.
async fn album_similar(
    ctx: &Ctx,
    album_id: &str,
    artist_id: Option<&str>,
    seeds: &[String],
) -> Result<Vec<SimilarEntry>, String> {
    let exclude_albums = vec![album_id.to_owned()];
    let exclude_artists: Vec<String> = artist_id.map(|a| vec![a.to_owned()]).unwrap_or_default();

    let (albums_res, artists_res) = futures_util::future::join(
        api::similar_albums(&ctx.config, seeds, &exclude_albums, SIMILAR_N),
        api::similar_artists(&ctx.config, seeds, &exclude_artists, SIMILAR_N),
    )
    .await;
    // Each half degrades to empty on its own error — a 5xx on one must not
    // discard a successful other half (the footer is best-effort).
    let albums = similar_groups(albums_res);
    let artists = similar_groups(artists_res);

    let client = ctx.subsonic().await.map_err(|e| e.to_string())?;
    let album_futs = albums.iter().map(|g| async {
        client
            .get_album(&music_core::AlbumId::from(g.id.clone()))
            .await
            .ok()
            .map(|aws| SimilarEntry {
                kind: SimilarKind::Album,
                id: g.id.clone(),
                name: aws.album.name,
                artist: aws.album.artist_name,
            })
    });
    let artist_futs = artists.iter().map(|g| async {
        client
            .get_artist(&music_core::ArtistId::from(g.id.clone()))
            .await
            .ok()
            .map(|awa| SimilarEntry {
                kind: SimilarKind::Artist,
                id: g.id.clone(),
                name: awa.artist.name,
                artist: None,
            })
    });
    let (album_entries, artist_entries) = futures_util::future::join(
        futures_util::future::join_all(album_futs),
        futures_util::future::join_all(artist_futs),
    )
    .await;

    let mut out: Vec<SimilarEntry> = album_entries.into_iter().flatten().collect();
    out.extend(artist_entries.into_iter().flatten());
    Ok(out)
}

/// Map a `similar_*` result to its groups. Best-effort: "recommender not
/// ready", an all-unindexed seed set, *and* an unexpected error all degrade
/// to an empty half (logged) rather than failing the whole footer.
fn similar_groups(res: Result<api::SimilarList, ApiError>) -> Vec<api::SimilarGroup> {
    match res {
        Ok(list) if list.all_seeds_unindexed => Vec::new(),
        Ok(list) => list.groups,
        Err(ApiError::RecommenderUnavailable) => Vec::new(),
        Err(e) => {
            tracing::debug!(error = %e, "similar half failed; degrading to empty");
            Vec::new()
        }
    }
}

/// Fetch a playlist's ids and hydrate them to tracks for the detail pane.
async fn open_playlist(ctx: &Ctx, id: &str) -> Result<PlaylistDetailState, String> {
    let detail = api::get_playlist(&ctx.config, id)
        .await
        .map_err(|e| playlist_err(&e))?;
    let client = ctx.subsonic().await.map_err(|e| e.to_string())?;
    let track_ids: Vec<TrackId> = detail
        .track_ids
        .iter()
        .map(|s| TrackId::from(s.clone()))
        .collect();
    let (tracks, _failed) = api::resolve_tracks(&client, &track_ids).await;
    Ok(PlaylistDetailState {
        summary: detail.summary,
        tracks,
        track_ids: detail.track_ids,
    })
}

/// Create a playlist and, when `then_add` is set, append that track in the
/// same task (the picker's "new playlist…" path).
async fn playlist_create(ctx: &Ctx, name: &str, then_add: Option<&str>) -> Msg {
    let created = match api::create_playlist(&ctx.config, name).await {
        Ok(summary) => summary,
        Err(e) => return write_error(&e),
    };
    if let Some(track_id) = then_add {
        match api::put_playlist_tracks(&ctx.config, &created.id, &[track_id.to_owned()], true).await
        {
            Ok(()) => {}
            Err(e) => {
                return Msg::PlaylistWriteDone {
                    note: format!(
                        "created {name}, but adding the track failed: {}",
                        playlist_err(&e)
                    ),
                    is_error: true,
                    reload_list: true,
                    reopen_id: None,
                };
            }
        }
    }
    Msg::PlaylistWriteDone {
        note: format!("created {name}"),
        is_error: false,
        reload_list: true,
        reopen_id: None,
    }
}

/// A generic failed-write completion (no reopen, no list reload).
fn write_error(e: &ApiError) -> Msg {
    Msg::PlaylistWriteDone {
        note: format!("playlist write failed: {}", playlist_err(e)),
        is_error: true,
        reload_list: false,
        reopen_id: None,
    }
}

/// Human-readable text for a playlist `ApiError` — names the guest case.
fn playlist_err(e: &ApiError) -> String {
    match e {
        ApiError::Forbidden => "not permitted (guests can't modify playlists)".to_owned(),
        other => other.to_string(),
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

    // Resolve album + artist display names concurrently (best-effort — a
    // failed lookup just falls back to the id in the UI).
    let labels = resolve_entity_labels(&client, &ratings).await;

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
            label: labels.get(item.id.as_str()).cloned(),
        });
    }
    // Likes first, then dislikes — matches the classic `liked` sections.
    entries.sort_by_key(|e| matches!(e.rating, Rating::Dislike));
    Ok(entries)
}

/// Resolve `album`/`artist` rating rows to their display names, keyed by id.
/// Best-effort and concurrent — a missing/failed entity is simply absent
/// from the map (the UI falls back to the id).
async fn resolve_entity_labels(
    client: &Client,
    ratings: &[api::RatingItem],
) -> std::collections::HashMap<String, String> {
    let album_futs = ratings.iter().filter(|r| r.kind == "album").map(|r| async {
        client
            .get_album(&music_core::AlbumId::from(r.id.clone()))
            .await
            .ok()
            .map(|aws| (r.id.clone(), aws.album.name))
    });
    let artist_futs = ratings.iter().filter(|r| r.kind == "artist").map(|r| async {
        client
            .get_artist(&music_core::ArtistId::from(r.id.clone()))
            .await
            .ok()
            .map(|awa| (r.id.clone(), awa.artist.name))
    });
    let (albums, artists) = futures_util::future::join(
        futures_util::future::join_all(album_futs),
        futures_util::future::join_all(artist_futs),
    )
    .await;
    albums
        .into_iter()
        .flatten()
        .chain(artists.into_iter().flatten())
        .collect()
}
