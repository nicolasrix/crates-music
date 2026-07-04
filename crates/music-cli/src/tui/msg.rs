//! The TUI's message contract: `Msg` is everything that can happen (semantic
//! key actions, timer ticks, player events, async-effect completions);
//! `Effect` is everything the reducer can ask the runtime to do.
//!
//! The invariant that keeps the loop simple: **every spawned `Effect`
//! completes by sending exactly one `Msg`** back over the channel. Racy
//! fetches (albums / search / station) carry a generation counter stamped by
//! the reducer so a stale response can never overwrite newer state.

use bytes::Bytes;
use music_core::{AlbumId, Track};
use music_player::PlayerEvent;
use music_subsonic::{AlbumListType, AlbumWithSongs, SearchResult3};

use super::state::{LikedEntry, Rating, Section};

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
    QueueClear,

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
}
