//! Root TUI state. Everything the reducer mutates and the renderer reads
//! lives here — no terminal, no HTTP, so the whole tree is constructible in
//! unit tests.

use std::collections::HashMap;
use std::time::Duration;

use bytes::Bytes;
use music_core::{Album, Track};
use music_player::{PlayQueue, PlaybackSnapshot, Player, QueuedTrack};
use music_subsonic::{AlbumListType, AlbumWithSongs, SearchResult3};
use ratatui::widgets::TableState;

use super::widgets::input::InputField;

/// Sidebar sections, in display order (the `1..5` bindings index this).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Library,
    Search,
    Queue,
    Stations,
    Liked,
}

impl Section {
    pub(crate) const ALL: [Self; 5] = [
        Self::Library,
        Self::Search,
        Self::Queue,
        Self::Stations,
        Self::Liked,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Library => "Library",
            Self::Search => "Search",
            Self::Queue => "Queue",
            Self::Stations => "Stations",
            Self::Liked => "Liked",
        }
    }

    pub(crate) fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Overlay {
    None,
    Help,
}

/// Every remote-data slot renders all four of these states.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Loadable<T> {
    #[default]
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Loadable<T> {
    pub(crate) fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(v) => Some(v),
            _ => None,
        }
    }
}

/// A like/dislike verdict. `None` in an `Option<Rating>` means neutral.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rating {
    Like,
    Dislike,
}

impl Rating {
    pub(crate) fn wire(self) -> &'static str {
        match self {
            Self::Like => "like",
            Self::Dislike => "dislike",
        }
    }
}

/// One row of the Liked view: the rating plus (for tracks) the resolved
/// metadata. Albums/artists render by id only in v1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LikedEntry {
    pub kind: String,
    pub id: String,
    pub rating: Rating,
    pub track: Option<Track>,
}

/// Transient one-line message above the now-playing bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusLine {
    pub text: String,
    pub is_error: bool,
    /// Tick count (App.tick) after which the line disappears.
    pub expires_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LibraryPane {
    Albums,
    AlbumDetail,
}

/// Album-list kinds in `h`/`l` cycle order, with their tab labels.
pub(crate) const ALBUM_KINDS: [(AlbumListType, &str); 6] = [
    (AlbumListType::Newest, "newest"),
    (AlbumListType::Random, "random"),
    (AlbumListType::Frequent, "frequent"),
    (AlbumListType::Recent, "recent"),
    (AlbumListType::AlphabeticalByName, "by name"),
    (AlbumListType::AlphabeticalByArtist, "by artist"),
];

#[derive(Debug)]
pub(crate) struct LibraryState {
    pub kind_idx: usize,
    pub albums: Loadable<Vec<Album>>,
    pub albums_table: TableState,
    pub pane: LibraryPane,
    pub open_album: Loadable<AlbumWithSongs>,
    /// Album id the detail pane is waiting for — a completion for any other
    /// id is stale and dropped.
    pub open_target: Option<String>,
    pub tracks_table: TableState,
    pub generation: u64,
}

impl Default for LibraryState {
    fn default() -> Self {
        Self {
            kind_idx: 0,
            albums: Loadable::Idle,
            albums_table: TableState::default(),
            pane: LibraryPane::Albums,
            open_album: Loadable::Idle,
            open_target: None,
            tracks_table: TableState::default(),
            generation: 0,
        }
    }
}

impl LibraryState {
    pub(crate) fn kind(&self) -> AlbumListType {
        ALBUM_KINDS[self.kind_idx % ALBUM_KINDS.len()].0
    }
}

/// The three search result buckets, in Tab-cycle order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchBucket {
    Tracks,
    Albums,
    Artists,
}

impl SearchBucket {
    pub(crate) const ALL: [Self; 3] = [Self::Tracks, Self::Albums, Self::Artists];
}

#[derive(Debug, Default)]
pub(crate) struct SearchState {
    pub input: InputField,
    pub focused: bool,
    pub results: Loadable<SearchResult3>,
    pub bucket: usize,
    pub tables: [TableState; 3],
    pub generation: u64,
}

impl SearchState {
    pub(crate) fn bucket(&self) -> SearchBucket {
        SearchBucket::ALL[self.bucket % SearchBucket::ALL.len()]
    }
}

#[derive(Debug, Default)]
pub(crate) struct StationsState {
    pub input: InputField,
    pub focused: bool,
    pub last_prompt: String,
    pub results: Loadable<Vec<Track>>,
    pub table: TableState,
    pub generation: u64,
}

#[derive(Debug, Default)]
pub(crate) struct LikedState {
    pub entries: Loadable<Vec<LikedEntry>>,
    pub table: TableState,
}

/// Root state.
#[derive(Debug)]
pub(crate) struct App {
    pub section: Section,
    pub overlay: Overlay,
    pub should_quit: bool,
    /// Monotonic 250 ms tick counter (status expiry etc.).
    pub tick: u64,
    pub status: Option<StatusLine>,

    // ── playback ───────────────────────────────────────────────────────
    /// Handle to the audio thread; `None` when the device is missing (or
    /// in reducer tests). All access goes through the `player_*` helpers,
    /// which no-op on `None`.
    pub player: Option<Player>,
    pub no_audio_device: bool,
    /// Snapshot polled from the player each frame by the event loop.
    pub playback: PlaybackSnapshot,
    pub queue: PlayQueue,
    pub queue_table: TableState,
    /// Prefetched next-up bytes for near-gapless handoff.
    pub prefetched: Option<(String, Bytes)>,
    /// Queue index whose audio resolve is in flight (renders a spinner and
    /// guards stale `AudioReady` messages).
    pub pending_load: Option<usize>,

    // ── per-section view state ────────────────────────────────────────
    pub library: LibraryState,
    pub search: SearchState,
    pub stations: StationsState,
    pub liked: LikedState,

    /// Track/album/artist verdicts, applied optimistically; keyed by entity
    /// id (ids are globally unique across kinds in Navidrome).
    pub ratings: HashMap<String, Rating>,
}

impl App {
    pub(crate) fn new(player: Option<Player>, no_audio_device: bool) -> Self {
        Self {
            section: Section::Library,
            overlay: Overlay::None,
            should_quit: false,
            tick: 0,
            status: None,
            player,
            no_audio_device,
            playback: PlaybackSnapshot::default(),
            queue: PlayQueue::new(),
            queue_table: TableState::default(),
            prefetched: None,
            pending_load: None,
            library: LibraryState::default(),
            search: SearchState::default(),
            stations: StationsState::default(),
            liked: LikedState::default(),
            ratings: HashMap::new(),
        }
    }

    /// Is a text input focused (keys should type, not act)?
    pub(crate) fn input_focused(&self) -> bool {
        match self.section {
            Section::Search => self.search.focused,
            Section::Stations => self.stations.focused,
            _ => false,
        }
    }

    pub(crate) fn set_status(&mut self, text: impl Into<String>, is_error: bool) {
        self.status = Some(StatusLine {
            text: text.into(),
            is_error,
            // ~5 s at the 250 ms tick cadence.
            expires_at: self.tick + 20,
        });
    }

    // Player pass-throughs that tolerate a missing device.
    pub(crate) fn player_load(&self, bytes: Bytes, track_id: String, duration: Option<Duration>) {
        if let Some(p) = &self.player {
            p.load(bytes, track_id, duration);
        }
    }

    pub(crate) fn player_stop(&self) {
        if let Some(p) = &self.player {
            p.stop();
        }
    }
}

/// Convert a domain track to the queue's row type.
pub(crate) fn to_queued(t: &Track) -> QueuedTrack {
    QueuedTrack {
        id: t.id.as_str().to_owned(),
        title: t.title.clone(),
        artist: t.artist_name.clone(),
        album: t.album_name.clone(),
        duration: t.duration_seconds.map(u64::from).map(Duration::from_secs),
    }
}
