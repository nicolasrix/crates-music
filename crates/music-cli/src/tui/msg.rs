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
use music_core::{AlbumId, Track};
use music_player::PlayerEvent;
use music_subsonic::{AlbumListType, AlbumWithSongs, SearchResult3};
use music_sync::{ServerMessage, SyncOp};

use crate::api::{PlaylistSummary, WhoamiInfo};

use super::signal::PendingEvent;
use super::state::{
    ArtistDetailState, FeedbackVote, LikedEntry, PlaylistDetailState, Rating, Section, SimilarEntry,
};

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
    SeekBy(i64),
    VolumeBy(f32),
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
        /// Autoplay-added track ids (provenance) to exclude from seeds.
        recommended: Vec<String>,
        /// The session anchor track (weight-3 seed), when a room session exists.
        anchor_track_id: Option<String>,
        /// Recommend-session id for downvote scoping, when known.
        session_id: Option<String>,
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
}
