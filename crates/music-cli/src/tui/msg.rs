//! The TUI's message contract: `Msg` is everything that can happen (semantic
//! key actions, timer ticks, player events, async-effect completions);
//! `Effect` is everything the reducer can ask the runtime to do.
//!
//! The invariant that keeps the loop simple: **every spawned `Effect`
//! completes by sending exactly one `Msg`** back over the channel (the three
//! deliberately-silent exceptions: audio prefetch, scrobble, and sync-op
//! submit — see each variant's doc). Racy
//! fetches (albums / search / station) carry a generation counter stamped by
//! the reducer so a stale response can never overwrite newer state.

use bytes::Bytes;
use music_cache::AudioCacheStats;
use music_core::{AlbumId, Track};
use music_player::PlayerEvent;
use music_subsonic::{AlbumListType, AlbumWithSongs, SearchResult3};
use music_sync::{ServerMessage, SyncOp};

use crate::api::{
    ClientEvent, LatentNeighbour, LatentSpace, LyricsOutcome, PlaylistSummary, QueueDepth,
    RecentlyPlayed, RecommenderPanels, WhoamiInfo,
};
use crate::config::{AutoplayConfig, PlaybackConfig, Quality};

use super::signal::PendingEvent;
use super::state::{
    ArtistDetailState, DiagTab, DiagWindow, FeedbackVote, LikedEntry, PinnedRow,
    PlaylistDetailState, Rating, Section, SimilarEntry, TracingData,
};

/// The payload of a completed [`Effect::LoadDiagnostics`], one variant per
/// sub-tab (each tab fetches a different shape). Boxed where large so `Msg`
/// stays cheap to move.
#[derive(Debug)]
pub(crate) enum DiagData {
    Ingest(QueueDepth),
    Recommender(Box<RecommenderPanels>),
    Listening(Vec<RecentlyPlayed>),
    Tracing(TracingData),
    Latent(Box<LatentSpace>),
    ClientEvents(Vec<ClientEvent>),
}

/// Single-line text-field edits, produced by the keymap only while an input
/// is focused (so plain chars never trigger global bindings mid-typing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputMsg {
    Char(char),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
}

/// Recommender failures split into "not ready" (friendly panel) and real
/// errors (error panel) — mirrors `api::ApiError` minus the anyhow payload
/// so it stays `Clone` for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StationError {
    Unavailable,
    Other(String),
}

/// What the sync WS task (or an HTTP resync) reports back to the reducer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SyncEvent {
    /// A server frame arrived (`snapshot` / `applied` / `op_error`). The
    /// first frame after every (re)connect is a `Snapshot`, which is what
    /// flips the reducer online.
    Frame(ServerMessage),
    /// The connection is down (failed to connect, or dropped). The task
    /// keeps retrying with backoff; this just tells the reducer to run
    /// the queue locally in the meantime.
    Down { reason: String },
}

#[derive(Debug)]
pub(crate) enum Msg {
    // ── loop-generated ────────────────────────────────────────────────
    /// 250 ms heartbeat: progress bar refresh + status-line expiry.
    Tick,
    Player(PlayerEvent),

    // ── semantic key actions (produced by keymap.rs) ─────────────────
    Quit,
    ToggleHelp,
    /// Esc: close overlay → unfocus input → leave album detail, in that
    /// order of precedence (the reducer resolves the context).
    Back,
    GoSection(Section),
    NextSection,
    PrevSection,
    NavUp,
    NavDown,
    NavTop,
    NavBottom,
    NavHalfPageDown,
    NavHalfPageUp,
    /// h/l — cycle the library album-list kind.
    CycleKindPrev,
    CycleKindNext,
    /// [ / ] — cycle the library browse mode (albums / artists / tracks).
    CycleModePrev,
    CycleModeNext,
    /// S — start a station from the open album/artist (replaces the queue).
    AlbumStation,
    /// Enter — open/play/jump depending on section + selected row.
    Activate,
    /// e — enqueue the selected track (or album's tracks).
    Enqueue,
    /// '/' — jump to Search with the input focused.
    FocusSearch,
    /// i — focus the current section's input (stations prompt).
    FocusInput,
    Input(InputMsg),
    SubmitInput,
    TransportToggle,
    TransportNext,
    TransportPrev,
    /// Explicit resume / pause (distinct from the toggle) — emitted by the
    /// Linux MPRIS bridge, which receives separate Play and Pause D-Bus calls
    /// from desktop media controllers. Never bound to a key.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    TransportPlay,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    TransportPause,
    SeekBy(i64),
    VolumeBy(f32),
    /// M — mute/unmute toggle. Zeroes the volume, remembering the prior level
    /// to restore on the next press.
    ToggleMute,
    /// L / D / u — like / dislike / clear on the selected (or now-playing)
    /// track.
    Rate(Option<Rating>),
    /// r — recommend-next seeded from the now-playing track, enqueued.
    RecommendFromNowPlaying,
    QueueRemoveSelected,
    /// c — clear *upcoming* tracks, preserving the now-playing one
    /// (mirrors the web queue page; a full wipe was never what "clear
    /// the queue" meant mid-listen).
    QueueClear,
    /// J / K — move the selected queue row down / up one slot.
    QueueMoveDown,
    QueueMoveUp,
    /// T — move the selected queue row to the top.
    QueueMoveTop,
    /// P — play the selection next: in the queue view, moves the row to
    /// right after the cursor; in track lists, enqueues it there.
    PlayNext,
    /// o — toggle "play audio on this device" (sync rooms only).
    ToggleOutput,
    /// A — toggle tethered-drift autoplay (keeps the queue topped up).
    ToggleAutoplay,
    /// f / F — thumbs up / down on the now-playing autoplay pick
    /// (`/v1/recommend/feedback`); a no-op on user-picked tracks.
    Feedback(FeedbackVote),

    // ── downloads / offline ───────────────────────────────────────────
    /// d — toggle save-offline for the contextual track. The effect pins
    /// (fetch-if-missing) or unpins atomically based on current cache state.
    SaveOffline,
    /// W — bulk-download: pin a whole album/playlist, or (in the Downloads
    /// section) warm the cache from every liked track.
    BulkDownload,
    /// E — evict the regular cache down to its budget (Downloads only).
    EvictCache,

    // ── playlists ─────────────────────────────────────────────────────
    /// a — open the add-to-playlist picker for the contextual track (any
    /// section that lists tracks, including the queue).
    AddToPlaylist,
    /// N — create a new (empty) playlist from the Playlists list pane.
    NewPlaylist,
    /// s — shuffle-play the open playlist (Playlists detail pane).
    PlaylistShufflePlay,
    /// R — rename the open playlist (opens the text prompt).
    PlaylistRenamePrompt,
    /// X — delete the open playlist; two-step (a second `X` confirms).
    PlaylistDelete,
    /// x — remove the selected track from the open playlist (optimistic).
    PlaylistRemoveTrack,
    /// m — suggest more tracks for the open playlist (recommender).
    PlaylistSuggest,
    /// Picker overlay: move selection, activate a row, or dismiss.
    PickerMove(i8),
    PickerActivate,
    PickerClose,
    /// Text-prompt overlay: edit, submit, or dismiss.
    PromptInput(InputMsg),
    PromptSubmit,
    PromptClose,
    /// y — open/close the lyric pane for the now-playing track.
    ToggleLyrics,
    /// j/k inside the pane: read ahead or back, which stops it following
    /// the playhead until you seek or the track changes.
    LyricsMove(i32),
    /// Enter inside the pane: seek to the line under the cursor.
    LyricsSeek,
    /// R inside the pane: re-resolve server-side (the wrong-song escape
    /// hatch). Guests are refused by the gateway.
    LyricsRefresh,

    // ── effect completions ────────────────────────────────────────────
    AlbumsLoaded {
        generation: u64,
        result: Result<Vec<music_core::Album>, String>,
    },
    AlbumOpened {
        id: AlbumId,
        result: Result<AlbumWithSongs, String>,
    },
    AlbumTracksForEnqueue {
        result: Result<AlbumWithSongs, String>,
    },
    ArtistsLoaded {
        generation: u64,
        result: Result<Vec<music_core::Artist>, String>,
    },
    /// Full-library track page (Tracks mode).
    SongsLoaded {
        generation: u64,
        result: Result<Vec<Track>, String>,
    },
    ArtistOpened {
        id: String,
        result: Result<ArtistDetailState, String>,
    },
    /// "You might like" footer resolved for the open album.
    AlbumSimilarLoaded {
        album_id: String,
        result: Result<Vec<SimilarEntry>, String>,
    },
    /// Album/artist station tracks resolved — replaces the queue.
    AlbumStationDone {
        result: Result<Vec<Track>, StationError>,
    },
    SearchDone {
        generation: u64,
        result: Result<SearchResult3, String>,
    },
    StationDone {
        generation: u64,
        result: Result<Vec<Track>, StationError>,
    },
    RecommendDone {
        result: Result<Vec<Track>, StationError>,
    },
    LikedLoaded {
        result: Result<Vec<LikedEntry>, String>,
    },
    RatingSet {
        id: String,
        /// The pre-optimistic-update value, for rollback on failure.
        previous: Option<Rating>,
        result: Result<(), String>,
    },
    /// Audio bytes for the queue's current track are ready to load.
    AudioReady {
        queue_index: usize,
        track_id: String,
        bytes: Bytes,
    },
    AudioFailed {
        queue_index: usize,
        track_id: String,
        error: String,
    },
    /// Next-up bytes prefetched for near-gapless handoff.
    PrefetchReady {
        track_id: String,
        bytes: Bytes,
    },
    /// A batched `POST /v1/events` finished; `events` echoes what was sent
    /// so a failure can re-queue them (bounded by their `attempts`).
    EventsFlushed {
        events: Vec<PendingEvent>,
        result: Result<(), String>,
    },
    /// A sync WS frame or connection-state change (see [`SyncEvent`]).
    Sync(SyncEvent),
    PlaylistsLoaded {
        generation: u64,
        result: Result<Vec<PlaylistSummary>, String>,
    },
    PlaylistOpened {
        id: String,
        result: Result<PlaylistDetailState, String>,
    },
    /// A playlist write (create/rename/delete/add/set-tracks) finished.
    /// `reopen_id` reloads that playlist's detail (rename, or a failed
    /// optimistic remove that needs resyncing); `reload_list` refreshes the
    /// list pane (counts/membership changed).
    PlaylistWriteDone {
        note: String,
        is_error: bool,
        reload_list: bool,
        reopen_id: Option<String>,
    },
    PlaylistSuggestionsDone {
        playlist_id: String,
        result: Result<Vec<Track>, StationError>,
    },
    /// `GET /v1/whoami` completed (boot-time identity fetch).
    WhoamiLoaded {
        result: Result<WhoamiInfo, String>,
    },
    /// Lyrics resolved. Carries the track it was asked for so a document
    /// that lands after the track has moved on is dropped rather than
    /// shown against the wrong song.
    LyricsLoaded {
        track_id: String,
        result: Result<LyricsOutcome, String>,
    },
    /// Downloads section loaded: cache totals + the hydrated pinned table.
    /// The two halves fail independently — stats is local SQLite; hydration
    /// needs the server, so `pinned` still `Ok`s (with `track: None` rows)
    /// when offline.
    DownloadsLoaded {
        stats: Result<AudioCacheStats, String>,
        pinned: Result<Vec<PinnedRow>, String>,
    },
    /// A pin / unpin / bulk-download / evict op finished. The reducer sets the
    /// status note and, if the Downloads section is on screen, reloads it (pin
    /// set + byte totals moved).
    PinDone {
        note: String,
        is_error: bool,
    },
    /// An autoplay refill resolved. `need` was the shortfall requested (an
    /// under-delivery triggers the long cooldown); `generation` is dropped if
    /// autoplay was toggled since the request went out.
    AutoplayRefilled {
        generation: u64,
        need: usize,
        result: Result<Vec<Track>, StationError>,
    },
    /// A feedback thumb write finished; `previous` is the pre-optimistic vote
    /// for rollback on failure.
    FeedbackDone {
        track_id: String,
        previous: Option<FeedbackVote>,
        result: Result<(), String>,
    },
    /// Track metadata resolved for sync-queue hydration. `ids` echoes the
    /// request so failures can clear the in-flight set (retry happens on
    /// the next queue change, never in a hot loop).
    TracksHydrated {
        ids: Vec<String>,
        result: Result<Vec<Track>, String>,
    },

    // ── settings ──────────────────────────────────────────────────────
    /// A settings write to the config file finished (only surfaced on error —
    /// a silent success keeps the form quiet).
    SettingsSaved {
        result: Result<(), String>,
    },
    /// Sign-out finished (token revoked + store cleared). On success the
    /// reducer sets `exit_message` and quits to the auth-needed shell line.
    SignedOut {
        result: Result<(), String>,
    },
    /// `POST /v1/admin/cache/invalidate` finished (admin-only Settings action).
    CacheInvalidated {
        result: Result<(), String>,
    },

    // ── diagnostics (admin-only) ───────────────────────────────────────
    /// A diagnostics sub-tab load finished. `tab` echoes the request so a
    /// stale completion (the user switched tabs mid-fetch) can be dropped.
    DiagnosticsLoaded {
        tab: DiagTab,
        result: Result<DiagData, String>,
    },
    /// The latent-space nearest neighbours for the selected point resolved.
    /// `seed` echoes the point id so a stale completion is dropped.
    LatentNeighboursLoaded {
        seed: String,
        result: Result<Vec<LatentNeighbour>, String>,
    },
}

/// Full settings snapshot for the [`Effect::SaveSettings`] persist. Floats are
/// carried as `f32::to_bits` so `Effect` stays `Eq` (the no-floats-in-`Effect`
/// rule from autoplay). The effect rebuilds a `Config` from this plus its boot
/// base and writes it to disk, then applies quality + drift to the live cell
/// and the budgets to the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SettingsSave {
    pub stream_quality: Quality,
    pub download_quality: Quality,
    pub regular_budget_bytes: u64,
    pub pinned_budget_bytes: u64,
    /// Run an eviction after applying budgets (set when a budget was lowered).
    pub evict_now: bool,
    pub autoplay_enabled: bool,
    pub min_upcoming: usize,
    pub frontier_window: usize,
    pub leash_tau_bits: u32,
    pub leash_lambda_bits: u32,
    pub frontier_weight_bits: u32,
    pub frontier_decay_bits: u32,
    pub mmr_lambda_bits: u32,
}

impl SettingsSave {
    /// The playback (transcode) config this snapshot describes.
    pub(crate) fn playback(&self) -> PlaybackConfig {
        PlaybackConfig {
            stream_quality: self.stream_quality,
            download_quality: self.download_quality,
        }
    }

    /// The full autoplay config (drift floats decoded from their bit form).
    pub(crate) fn autoplay(&self) -> AutoplayConfig {
        AutoplayConfig {
            enabled: self.autoplay_enabled,
            min_upcoming: self.min_upcoming,
            leash_tau: f32::from_bits(self.leash_tau_bits),
            leash_lambda: f32::from_bits(self.leash_lambda_bits),
            frontier_weight: f32::from_bits(self.frontier_weight_bits),
            frontier_decay: f32::from_bits(self.frontier_decay_bits),
            frontier_window: self.frontier_window,
            mmr_lambda: f32::from_bits(self.mmr_lambda_bits),
        }
    }
}

/// Async work the reducer requests; `effects::spawn` runs each on tokio and
/// funnels the completion back as a `Msg`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Effect {
    LoadAlbums {
        generation: u64,
        kind: AlbumListType,
        size: u32,
    },
    OpenAlbum {
        id: AlbumId,
    },
    EnqueueAlbum {
        id: AlbumId,
    },
    /// `getArtists`; completes as [`Msg::ArtistsLoaded`].
    LoadArtists {
        generation: u64,
    },
    /// Empty `search3` page over the whole library; completes as
    /// [`Msg::SongsLoaded`].
    LoadSongs {
        generation: u64,
    },
    /// `getArtist` + `getTopSongs`; completes as [`Msg::ArtistOpened`].
    /// `name` is needed because `getTopSongs` keys on artist name, not id.
    OpenArtist {
        id: String,
        name: String,
    },
    /// `similar_albums` + `similar_artists`, hydrated to names; completes as
    /// [`Msg::AlbumSimilarLoaded`].
    LoadAlbumSimilar {
        album_id: String,
        artist_id: Option<String>,
        seed_track_ids: Vec<String>,
    },
    /// `from-any` seeded by the given candidate track ids, resolved to
    /// tracks; completes as [`Msg::AlbumStationDone`].
    AlbumStation {
        candidate_seeds: Vec<String>,
    },
    Search {
        generation: u64,
        query: String,
    },
    Station {
        generation: u64,
        prompt: String,
        n: usize,
    },
    RecommendNext {
        seed: String,
        n: usize,
    },
    LoadLiked,
    SetRating {
        kind: &'static str,
        id: String,
        verdict: Option<Rating>,
        previous: Option<Rating>,
    },
    ResolveAudio {
        queue_index: usize,
        track_id: String,
    },
    PrefetchAudio {
        track_id: String,
    },
    /// `/rest/scrobble` — now-playing hint or play submission. The second
    /// deliberate exception to the one-`Msg`-per-effect invariant (after
    /// prefetch): a failed scrobble is logged, never surfaced — playback
    /// must not grow a status line because Navidrome hiccuped.
    Scrobble {
        track_id: String,
        submission: bool,
    },
    /// Batched `POST /v1/events` upload; completes as [`Msg::EventsFlushed`].
    FlushEvents {
        events: Vec<PendingEvent>,
    },
    /// Send one op up the sync WS. The third deliberate exception to the
    /// one-`Msg`-per-effect invariant: it's a channel send into the WS
    /// task (like the `Player` command sends), and every outcome already
    /// comes back through the socket — `Applied`/`OpError` frames, or a
    /// `SyncEvent::Down` if the connection is gone.
    SyncSubmit {
        op: SyncOp,
    },
    /// `GET /v1/sync/snapshot` to re-converge after a missed WS frame;
    /// completes as [`Msg::Sync`] (a `Snapshot` frame, or `Down` on error).
    SyncResync,
    /// `GET /v1/whoami`; completes as [`Msg::WhoamiLoaded`].
    LoadWhoami,
    /// `GET /v1/lyrics/:id`, or `POST …/refresh` when `force`; completes as
    /// [`Msg::LyricsLoaded`].
    LoadLyrics {
        track_id: String,
        force: bool,
    },
    /// Resolve track metadata for sync-queue items we didn't push
    /// ourselves; completes as [`Msg::TracksHydrated`].
    HydrateTracks {
        ids: Vec<String>,
    },
    /// Autoplay refill. Carries the Eq-friendly seed *inputs* (the weighting
    /// itself — floats — is computed in the effect via `tui::autoplay` +
    /// `[tui.autoplay]` config, so the `Effect` stays `Eq`). The effect builds
    /// the weighted seeds, falls back to `from-any` over the reversed queue
    /// when they're empty, and resolves ids to tracks. Completes as
    /// [`Msg::AutoplayRefilled`].
    AutoplayRefill {
        queue_track_ids: Vec<String>,
        now_playing_index: usize,
        /// Autoplay-added track ids (provenance) to exclude from seeds. A
        /// `HashSet` (order-independent, still `Eq`) so the effect uses it
        /// directly without a sort + rebuild.
        recommended: std::collections::HashSet<String>,
        /// The session anchor track (weight-3 seed), when a session exists.
        anchor_track_id: Option<String>,
        /// Session id for downvote scoping — the room anchor's, else the
        /// per-process feedback session (so it's always present).
        session_id: String,
        need: usize,
        generation: u64,
    },
    /// `POST /v1/recommend/feedback` — a thumb write; completes as
    /// [`Msg::FeedbackDone`]. `vote = None` clears the vote.
    SubmitFeedback {
        track_id: String,
        vote: Option<FeedbackVote>,
        session_id: String,
        previous: Option<FeedbackVote>,
    },

    // ── playlists ─────────────────────────────────────────────────────
    /// `GET /v1/playlists`; completes as [`Msg::PlaylistsLoaded`].
    LoadPlaylists {
        generation: u64,
    },
    /// `GET /v1/playlists/:id` + hydrate; completes as [`Msg::PlaylistOpened`].
    OpenPlaylist {
        id: String,
    },
    /// `POST /v1/playlists` (+ optional append of `then_add`); completes as
    /// [`Msg::PlaylistWriteDone`].
    PlaylistCreate {
        name: String,
        then_add: Option<String>,
    },
    /// `PATCH /v1/playlists/:id { name }`; completes as [`Msg::PlaylistWriteDone`].
    PlaylistRename {
        id: String,
        name: String,
    },
    /// `DELETE /v1/playlists/:id`; completes as [`Msg::PlaylistWriteDone`].
    PlaylistDelete {
        id: String,
    },
    /// `PUT /v1/playlists/:id/tracks` append; completes as [`Msg::PlaylistWriteDone`].
    PlaylistAddTrack {
        id: String,
        track_id: String,
    },
    /// `PUT /v1/playlists/:id/tracks` replace (remove/reorder); completes as
    /// [`Msg::PlaylistWriteDone`].
    PlaylistSetTracks {
        id: String,
        track_ids: Vec<String>,
    },
    /// `POST /v1/recommend/from-seeds`; completes as
    /// [`Msg::PlaylistSuggestionsDone`].
    PlaylistSuggest {
        playlist_id: String,
        seeds: Vec<String>,
    },

    // ── downloads / offline ───────────────────────────────────────────
    /// `stats` + `list_pinned` (local SQLite) + best-effort hydration of the
    /// pinned ids to tracks; completes as [`Msg::DownloadsLoaded`].
    LoadDownloads,
    /// Toggle offline-pin for one track: fetch-if-missing then pin, or unpin
    /// if it was already pinned. Completes as [`Msg::PinDone`].
    PinToggle {
        track_id: String,
        title: String,
    },
    /// Bulk-pin a set of tracks (album / playlist "download all"),
    /// fetch-if-missing, tolerant per track. Completes as [`Msg::PinDone`].
    PinBulk {
        track_ids: Vec<String>,
        label: String,
    },
    /// Warm the offline cache from every liked *track* (the Downloads `W`);
    /// fetches ratings server-side, then bulk-pins. Completes as
    /// [`Msg::PinDone`].
    WarmLiked,
    /// `evict_lru_to_fit` — fit the regular cache to its budget now.
    /// Completes as [`Msg::PinDone`].
    EvictCache,

    // ── settings ──────────────────────────────────────────────────────
    /// Persist the settings to the config file and apply them live (transcode
    /// quality + drift params to the effect cell, budgets to the cache, with
    /// an eviction when lowered). Completes as [`Msg::SettingsSaved`].
    SaveSettings(SettingsSave),
    /// Revoke the refresh token and delete the local token store; completes as
    /// [`Msg::SignedOut`].
    SignOut,
    /// `POST /v1/admin/cache/invalidate` (admin-gated); completes as
    /// [`Msg::CacheInvalidated`].
    CacheInvalidate,

    // ── diagnostics (admin-only) ───────────────────────────────────────
    /// Load one diagnostics sub-tab for the given window; completes as
    /// [`Msg::DiagnosticsLoaded`]. The effect resolves `window` to a
    /// `since_ms` cutoff at run time (keeps `Effect` `Eq`, no wall clock).
    LoadDiagnostics {
        tab: DiagTab,
        window: DiagWindow,
    },
    /// `recommend/latent_neighbours` for the selected scatter point (+ hydrate
    /// titles); completes as [`Msg::LatentNeighboursLoaded`].
    LoadLatentNeighbours {
        track_id: String,
    },
}
