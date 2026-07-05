//! Root TUI state. Everything the reducer mutates and the renderer reads
//! lives here — no terminal, no HTTP, so the whole tree is constructible in
//! unit tests.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use bytes::Bytes;
use music_cache::AudioCacheStats;
use music_core::{Album, Artist, Track};
use music_player::{PlayQueue, PlaybackSnapshot, Player, QueuedTrack};
use music_subsonic::{AlbumListType, AlbumWithSongs, SearchResult3};
use music_sync::SyncState;
use ratatui::widgets::TableState;

use crate::api::{PlaylistSummary, WhoamiInfo};

use super::signal::{PendingEvent, TrackSignal};
use super::widgets::input::InputField;

/// Sidebar sections, in display order (the `1..9` number bindings index
/// this). Playlists sits at slot 4, between Queue and Stations, Downloads at
/// slot 7, and Settings at slot 8 (Diagnostics lands later), per the parity
/// plan's target sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Library,
    Search,
    Queue,
    Playlists,
    Stations,
    Liked,
    Downloads,
    Settings,
}

impl Section {
    pub(crate) const ALL: [Self; 8] = [
        Self::Library,
        Self::Search,
        Self::Queue,
        Self::Playlists,
        Self::Stations,
        Self::Liked,
        Self::Downloads,
        Self::Settings,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Library => "Library",
            Self::Search => "Search",
            Self::Queue => "Queue",
            Self::Playlists => "Playlists",
            Self::Stations => "Stations",
            Self::Liked => "Liked",
            Self::Downloads => "Downloads",
            Self::Settings => "Settings",
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
    /// Modal list of the caller's playlists (+ a "new playlist" row) for the
    /// "add this track to a playlist" gesture. State in [`App::picker`].
    PlaylistPicker,
    /// Modal single-line text entry (create / rename a playlist). State in
    /// [`App::text_prompt`].
    TextPrompt,
}

/// Where the queue's source of truth lives right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncPhase {
    /// No gateway configured — the queue is local, permanently.
    Disabled,
    /// Gateway configured but the sync WS isn't delivering (connecting,
    /// or dropped). The queue operates locally; a reconnect adopts the
    /// server snapshot.
    Offline,
    /// Live room replica: queue gestures become ops over the WS and the
    /// local queue is a projection of the last server-confirmed state.
    Online,
}

/// The sync-room replica plus the client-side follow bookkeeping. Mirrors
/// the web's `SyncContext` + `PlayerContext` refs: the room state is what
/// the server last confirmed (echo-driven — no optimistic queue apply),
/// and the small `Option<String>` markers reproduce the web's auto-skip /
/// direct-pick semantics.
#[derive(Debug)]
pub(crate) struct RoomState {
    pub phase: SyncPhase,
    /// Last server-confirmed state; meaningful while `phase == Online`.
    pub room: SyncState,
    /// Track metadata by track id — the room queue stores only ids.
    pub meta: HashMap<String, QueuedTrack>,
    /// Metadata fetches in flight (dedups hydration requests).
    pub hydrating: HashSet<String>,
    /// Ids whose hydration `getSong` failed — held out of re-request so a
    /// deleted/unresolvable id can't hot-loop a fetch on every inbound
    /// frame. Cleared when a queue-growth op could reintroduce it.
    pub hydrate_failed: HashSet<String>,
    /// "Play audio on this device." Starts off so joining a room that is
    /// mid-playback doesn't blast audio; picking a track locally enables
    /// it (that gesture *means* "play here"), `o` toggles it.
    pub output_on: bool,
    /// Last cursor track id the auto-skip classifier saw — dislike
    /// auto-skip fires only when the cursor lands on a *new* track.
    pub last_classified: Option<String>,
    /// Track id the user explicitly picked — exempt from auto-skip once.
    pub direct_play: Option<String>,
    /// +1 normally, -1 while stepping backward — the direction auto-skip
    /// walks to find a playable track.
    pub advance_dir: i8,
}

impl RoomState {
    pub(crate) fn new(gateway: bool) -> Self {
        Self {
            phase: if gateway {
                SyncPhase::Offline
            } else {
                SyncPhase::Disabled
            },
            room: SyncState::new(),
            meta: HashMap::new(),
            hydrating: HashSet::new(),
            hydrate_failed: HashSet::new(),
            output_on: false,
            last_classified: None,
            direct_play: None,
            advance_dir: 1,
        }
    }

    pub(crate) fn online(&self) -> bool {
        self.phase == SyncPhase::Online
    }

    /// Short header indicator + whether it's a warning, or `None` in
    /// direct mode (no room to show state for).
    pub(crate) fn indicator(&self) -> Option<(&'static str, bool)> {
        match self.phase {
            SyncPhase::Disabled => None,
            SyncPhase::Offline => Some(("⚠ sync offline", true)),
            SyncPhase::Online if self.output_on => Some(("◉ synced", false)),
            // Online but not this device's audio — a silent remote.
            SyncPhase::Online => Some(("◉ synced · silent", false)),
        }
    }

    /// The room's session anchor. Read from playback state directly (not
    /// gated on `online()`) so it survives a transient WS drop — autoplay's
    /// weight-3 seed and the feedback session id stay stable across a blip,
    /// matching the web, which reads `session_anchor` regardless of
    /// connection state. `None` only when no session was ever started (direct
    /// mode, or before the first play).
    pub(crate) fn session_anchor(&self) -> Option<&music_core::SessionAnchor> {
        self.room.playback.session_anchor.as_ref()
    }

    /// Track id under the room's now-playing cursor, if any.
    pub(crate) fn cursor_track_id(&self) -> Option<&str> {
        let i = self.room.playback.now_playing_index?;
        self.room
            .playback
            .queue
            .items
            .get(i)
            .map(|it| it.track_id.as_str())
    }
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

/// One row of the Liked view: the rating plus resolved metadata. Tracks
/// carry the full [`Track`] (playable); albums/artists carry a resolved
/// display `label` (their name) and are navigable to their detail pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LikedEntry {
    pub kind: String,
    pub id: String,
    pub rating: Rating,
    pub track: Option<Track>,
    /// Resolved name for album/artist rows (via `getAlbum`/`getArtist`);
    /// `None` for tracks (use `track.title`) or if resolution failed.
    pub label: Option<String>,
}

/// A recommendation-feedback vote — thumbs on an autoplay-picked track.
/// Distinct from a library [`Rating`]: it's session-scoped taste signal
/// (`/v1/recommend/feedback`), not a persisted like/dislike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FeedbackVote {
    Up,
    Down,
}

impl FeedbackVote {
    pub(crate) fn wire(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

/// Tethered-drift autoplay: the toggle, provenance of autoplay-added tracks,
/// optimistic thumb votes, and the refill in-flight/cooldown bookkeeping.
/// All reducer-owned — the refill and feedback I/O ride effects.
#[derive(Debug)]
pub(crate) struct AutoplayState {
    pub enabled: bool,
    /// Refill threshold from `[tui.autoplay]`. The drift knobs (leash/frontier/
    /// mmr) aren't mirrored here — the effect reads those off the config so
    /// there's a single source of truth. Runtime editing lands in Phase 7.
    pub min_upcoming: usize,
    /// Track ids autoplay pushed. The seed builder's "skip algo-added" and the
    /// feedback-thumb gate both key on this; pruned to what's still in the
    /// queue on each refill so a track the user later re-queues by hand isn't
    /// mistaken for an autoplay pick.
    pub recommended: HashSet<String>,
    /// Track ids pushed by a refill but not yet reflected in the queue
    /// projection (the sync echo is in flight). Counted toward "upcoming" so a
    /// slow echo can't trigger a second, duplicate refill.
    pub pending_push: HashSet<String>,
    /// Optimistic thumb per track id (the server persists one per session).
    pub votes: HashMap<String, FeedbackVote>,
    /// Track ids with a feedback write in flight — a second thumb for the same
    /// track is refused until it lands, so votes serialize (like the web's
    /// `pending` guard) and a failed-write rollback can't clobber a newer vote.
    pub feedback_inflight: HashSet<String>,
    /// A refill effect is in flight — the sole re-entrancy guard.
    pub inflight: bool,
    /// Tick before which no refill fires (post-refill / underdelivery cooldown).
    pub cooldown_until: u64,
    /// Bumped on every toggle; a refill completion whose generation no longer
    /// matches is dropped (the "turned off mid-flight" guard).
    pub generation: u64,
    /// Per-process feedback session id — the fallback when no session anchor
    /// exists (direct mode / before first play), mirroring the web's RUM
    /// session. Used identically by refills and feedback so a downvote and the
    /// refill that could exclude it share one `(track, session)` bucket.
    pub feedback_session: String,
}

impl AutoplayState {
    fn new() -> Self {
        Self {
            enabled: false,
            min_upcoming: crate::config::AutoplayConfig::default().min_upcoming,
            recommended: HashSet::new(),
            pending_push: HashSet::new(),
            votes: HashMap::new(),
            feedback_inflight: HashSet::new(),
            inflight: false,
            cooldown_until: 0,
            generation: 0,
            feedback_session: crate::sync::new_session_id(),
        }
    }
}

/// Transient one-line message above the now-playing bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusLine {
    pub text: String,
    pub is_error: bool,
    /// Tick count (App.tick) after which the line disappears.
    pub expires_at: u64,
}

/// Which pane of the Library section is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LibraryPane {
    /// The mode-dependent browse list (albums / artists / tracks).
    Browse,
    /// One album's track list (+ station / similar footer).
    AlbumDetail,
    /// One artist's albums + top songs.
    ArtistDetail,
}

/// What the [`LibraryPane::Browse`] list shows, cycled with `[`/`]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LibraryMode {
    #[default]
    Albums,
    Artists,
    Tracks,
}

/// Browse modes in `[`/`]` cycle order, with their tab labels.
pub(crate) const LIBRARY_MODES: [(LibraryMode, &str); 3] = [
    (LibraryMode::Albums, "albums"),
    (LibraryMode::Artists, "artists"),
    (LibraryMode::Tracks, "tracks"),
];

/// Album-list kinds in `h`/`l` cycle order (Albums mode only), with labels.
pub(crate) const ALBUM_KINDS: [(AlbumListType, &str); 6] = [
    (AlbumListType::Newest, "newest"),
    (AlbumListType::Random, "random"),
    (AlbumListType::Frequent, "frequent"),
    (AlbumListType::Recent, "recent"),
    (AlbumListType::AlphabeticalByName, "by name"),
    (AlbumListType::AlphabeticalByArtist, "by artist"),
];

/// One row of the artist-detail list: an album (opens the album), or one of
/// the artist's top songs (plays / enqueues). A flat vector so the cursor
/// indexes a single table; `albums_len` marks the album/song boundary.
#[derive(Debug, Clone)]
pub(crate) enum ArtistRow {
    Album(Album),
    Song(Track),
}

#[derive(Debug, Clone)]
pub(crate) struct ArtistDetailState {
    pub artist: Artist,
    pub rows: Vec<ArtistRow>,
    /// `rows[..albums_len]` are albums; the remainder are top songs.
    pub albums_len: usize,
}

impl ArtistDetailState {
    /// The artist's top songs (the song rows), in order — the play/enqueue
    /// unit for the artist.
    pub(crate) fn top_songs(&self) -> Vec<Track> {
        self.rows
            .iter()
            .filter_map(|r| match r {
                ArtistRow::Song(t) => Some(t.clone()),
                ArtistRow::Album(_) => None,
            })
            .collect()
    }
}

/// Kind of a "you might like" footer entry on the album-detail pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SimilarKind {
    Album,
    Artist,
}

/// One hydrated "you might like" row (a similar album or artist), navigable
/// with `enter`.
#[derive(Debug, Clone)]
pub(crate) struct SimilarEntry {
    pub kind: SimilarKind,
    pub id: String,
    pub name: String,
    /// The album's artist name (for album rows), else `None`.
    pub artist: Option<String>,
}

#[derive(Debug)]
pub(crate) struct LibraryState {
    pub mode: LibraryMode,

    // ── Albums mode ──
    pub kind_idx: usize,
    pub albums: Loadable<Vec<Album>>,
    pub albums_table: TableState,

    // ── Artists mode ──
    pub artists: Loadable<Vec<Artist>>,
    pub artists_table: TableState,

    // ── Tracks mode (full library, first page) ──
    pub songs: Loadable<Vec<Track>>,
    pub songs_table: TableState,

    // ── panes ──
    pub pane: LibraryPane,
    pub open_album: Loadable<AlbumWithSongs>,
    /// Album id the detail pane is waiting for — a completion for any other
    /// id is stale and dropped.
    pub open_target: Option<String>,
    pub tracks_table: TableState,
    /// "You might like" footer for the open album (similar albums + artists).
    pub album_similar: Loadable<Vec<SimilarEntry>>,
    pub open_artist: Loadable<ArtistDetailState>,
    /// Artist id the artist-detail pane is waiting for (same guard idea).
    pub artist_target: Option<String>,
    pub artist_table: TableState,

    /// Where `esc` returns to when leaving a detail pane: the (section, pane)
    /// captured at the moment the detail was opened. Lets a detail opened
    /// from Search / Liked / a parent detail pane back out to its origin
    /// instead of always dumping the user in the Library browse list. Single
    /// slot (not a stack), so paths deeper than one level fall back to Browse.
    pub nav_return: Option<(Section, LibraryPane)>,

    /// Bumped on every browse-list (re)load; stamped on the load effect so a
    /// stale response for a mode we've since left can't overwrite state.
    pub generation: u64,
}

impl Default for LibraryState {
    fn default() -> Self {
        Self {
            mode: LibraryMode::default(),
            kind_idx: 0,
            albums: Loadable::Idle,
            albums_table: TableState::default(),
            artists: Loadable::Idle,
            artists_table: TableState::default(),
            songs: Loadable::Idle,
            songs_table: TableState::default(),
            pane: LibraryPane::Browse,
            open_album: Loadable::Idle,
            open_target: None,
            tracks_table: TableState::default(),
            album_similar: Loadable::Idle,
            open_artist: Loadable::Idle,
            artist_target: None,
            artist_table: TableState::default(),
            nav_return: None,
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

/// One row of the Downloads pinned table: a pinned cache entry, with its
/// track metadata hydrated when the server is reachable. `track` is `None`
/// offline (or for an unresolvable id) — the row still renders (and plays,
/// keyed on `track_id`) from the id + byte size alone, which is the whole
/// point of the offline story.
#[derive(Debug, Clone)]
pub(crate) struct PinnedRow {
    pub track_id: String,
    pub bytes: u64,
    pub track: Option<Track>,
}

impl PinnedRow {
    /// Display title: the hydrated track's title, else the raw id (offline).
    /// The single home of the id-fallback rule (view, queue entry, target).
    pub(crate) fn title_or_id(&self) -> String {
        self.track
            .as_ref()
            .map_or_else(|| self.track_id.clone(), |t| t.title.clone())
    }
}

/// Section 7 — offline downloads. Cache byte totals (the two-budget gauges)
/// plus the pinned-track table. Both reload on every visit: the underlying
/// SQLite reads are local and cheap, and pin state changes out from under us
/// (a `d` elsewhere, a background auto-cache).
// Not `#[derive(Default)]`: `Loadable`'s derived `Default` over-bounds with
// `T: Default`, and `AudioCacheStats` isn't `Default`.
#[derive(Debug)]
pub(crate) struct DownloadsState {
    pub stats: Loadable<AudioCacheStats>,
    pub pinned: Loadable<Vec<PinnedRow>>,
    pub table: TableState,
}

impl Default for DownloadsState {
    fn default() -> Self {
        Self {
            stats: Loadable::Idle,
            pinned: Loadable::Idle,
            table: TableState::default(),
        }
    }
}

/// One editable row of the Settings section, in display order within its
/// group. The interactive rows only — the account-card lines aren't selectable.
/// `InvalidateCache` is appended for admins alone (see `settings::rows`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingRow {
    // Playback
    StreamQuality,
    DownloadQuality,
    OutputDevice,
    // Storage
    RegularBudget,
    PinnedBudget,
    // Autoplay
    AutoplayEnabled,
    MinUpcoming,
    LeashTau,
    LeashLambda,
    FrontierWeight,
    FrontierDecay,
    FrontierWindow,
    MmrLambda,
    ResetAutoplay,
    // Account
    SignOut,
    // Admin (admins only)
    InvalidateCache,
}

/// Working copy of the settings the view edits. Quality, budgets, and the
/// autoplay *drift* params live here (they have no other home in `App`);
/// `enabled` / `min_upcoming` stay authoritative on [`AutoplayState`] and
/// output-on-device on `RoomState::output_on`, so the rows read/write those
/// directly. Every edit persists to the config file and applies live — see
/// `update::settings`. Seeded from disk by [`App::configure_settings`].
#[derive(Debug)]
pub(crate) struct SettingsState {
    pub stream_quality: crate::config::Quality,
    pub download_quality: crate::config::Quality,
    pub regular_budget_bytes: u64,
    pub pinned_budget_bytes: u64,
    pub leash_tau: f32,
    pub leash_lambda: f32,
    pub frontier_weight: f32,
    pub frontier_decay: f32,
    pub frontier_window: usize,
    pub mmr_lambda: f32,
    pub table: TableState,
    /// Sign-out is two-step (a stray `enter` shouldn't nuke the session): the
    /// first `enter` arms this, the second confirms. Any other action disarms.
    pub confirm_signout: bool,
}

impl Default for SettingsState {
    fn default() -> Self {
        let ap = crate::config::AutoplayConfig::default();
        let cache = crate::config::CacheConfig::default();
        Self {
            stream_quality: crate::config::Quality::default(),
            download_quality: crate::config::Quality::default(),
            regular_budget_bytes: cache.regular_budget_bytes,
            pinned_budget_bytes: cache.pinned_budget_bytes,
            leash_tau: ap.leash_tau,
            leash_lambda: ap.leash_lambda,
            frontier_weight: ap.frontier_weight,
            frontier_decay: ap.frontier_decay,
            frontier_window: ap.frontier_window,
            mmr_lambda: ap.mmr_lambda,
            table: TableState::default(),
            confirm_signout: false,
        }
    }
}

/// Which pane of the Playlists section is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PlaylistsPane {
    /// The list of playlists.
    #[default]
    List,
    /// One playlist's tracks.
    Detail,
    /// Recommender "suggest more" results for the open playlist (`m`).
    Suggestions,
}

/// A loaded playlist: its summary, the hydrated tracks (may be fewer than
/// `track_ids` if some ids didn't resolve), and the **raw ordered ids**.
/// Membership edits replace against `track_ids`, never the hydrated subset.
#[derive(Debug, Clone)]
pub(crate) struct PlaylistDetailState {
    pub summary: PlaylistSummary,
    pub tracks: Vec<Track>,
    pub track_ids: Vec<String>,
}

#[derive(Debug, Default)]
pub(crate) struct PlaylistsState {
    pub list: Loadable<Vec<PlaylistSummary>>,
    pub list_table: TableState,
    pub pane: PlaylistsPane,
    pub open: Loadable<PlaylistDetailState>,
    /// Playlist id the detail pane is waiting for — a completion for any
    /// other id is stale and dropped (mirrors `LibraryState::open_target`).
    pub open_id: Option<String>,
    pub detail_table: TableState,
    pub suggestions: Loadable<Vec<Track>>,
    pub suggest_table: TableState,
    /// Bumped on every list reload; stamped on the load effect so a stale
    /// response can't overwrite a newer one.
    pub generation: u64,
    /// Two-step delete: the playlist id awaiting a confirming second `X`,
    /// and the tick after which that confirmation lapses.
    pub pending_delete: Option<String>,
    pub delete_deadline: u64,
}

/// Modal playlist picker (the "add this track to a playlist" gesture, `a`).
/// The selectable rows are the caller's owned playlists followed by a
/// synthetic "new playlist…" row.
#[derive(Debug)]
pub(crate) struct PickerState {
    pub track_id: String,
    pub track_title: String,
    pub table: TableState,
}

/// What a [`TextPrompt`] does with its submitted text.
#[derive(Debug, Clone)]
pub(crate) enum PromptPurpose {
    /// Create an empty playlist with the entered name.
    CreatePlaylist,
    /// Create a playlist and immediately add a track to it (the picker's
    /// "new playlist…" path).
    CreatePlaylistThenAdd { track_id: String },
    /// Rename an existing playlist.
    RenamePlaylist { id: String },
}

/// Modal single-line text entry, reused for create + rename.
#[derive(Debug)]
pub(crate) struct TextPrompt {
    pub purpose: PromptPurpose,
    pub title: String,
    pub input: InputField,
}

/// Root state.
// The bools are independent facts about the session (device present, quit
// requested, flush in flight, gateway configured), not an implicit state
// machine — an enum would obscure, not clarify.
#[allow(clippy::struct_excessive_bools)]
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
    pub downloads: DownloadsState,
    pub playlists: PlaylistsState,
    /// Active playlist picker (add-to-playlist overlay), when open.
    pub picker: Option<PickerState>,
    /// Active text-entry modal (create / rename playlist), when open.
    pub text_prompt: Option<TextPrompt>,

    /// Track/album/artist verdicts, applied optimistically; keyed by entity
    /// id (ids are globally unique across kinds in Navidrome).
    pub ratings: HashMap<String, Rating>,

    // ── listening signal ──────────────────────────────────────────────
    /// Scrobble/skip emission state for the currently-loaded track.
    pub signal: Option<TrackSignal>,
    /// Events awaiting the next batched `POST /v1/events` flush.
    pub events_outbox: Vec<PendingEvent>,
    /// A flush effect is in flight — don't start another.
    pub events_inflight: bool,
    /// `/v1/events` is gateway-only; direct-to-Navidrome mode disables the
    /// outbox entirely (scrobbles still go out — they're Subsonic).
    pub events_enabled: bool,

    // ── sync room ─────────────────────────────────────────────────────
    /// Sync-room replica + follow bookkeeping. When online, `queue` above
    /// is a *projection* of `sync.room` and every queue gesture becomes a
    /// submitted op; when offline/disabled, `queue` is the truth.
    pub sync: RoomState,
    /// Who the gateway says we are (`GET /v1/whoami`); `None` until the
    /// boot fetch lands (or in direct mode).
    pub whoami: Option<WhoamiInfo>,

    // ── autoplay ──────────────────────────────────────────────────────
    /// Tethered-drift autoplay: toggle, provenance, votes, refill state.
    pub autoplay: AutoplayState,

    // ── settings ──────────────────────────────────────────────────────
    /// The Settings section's editable working copy (quality / budgets /
    /// drift params). Seeded from disk by [`App::configure_settings`].
    pub settings: SettingsState,
    /// The gateway (or, in direct mode, Navidrome) URL — shown in the
    /// Settings account card. Seeded by [`App::configure_settings`].
    pub server_url: Option<String>,
    /// Set when the user signs out: the event loop exits and prints this on
    /// the restored terminal (the "auth-needed screen" — a shell line telling
    /// them to `crates-cli auth login`).
    pub exit_message: Option<String>,
}

impl App {
    /// `gateway` = a `[gateway]` block is configured — it enables both the
    /// `/v1/events` outbox and the sync-room integration.
    pub(crate) fn new(player: Option<Player>, no_audio_device: bool, gateway: bool) -> Self {
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
            downloads: DownloadsState::default(),
            playlists: PlaylistsState::default(),
            picker: None,
            text_prompt: None,
            ratings: HashMap::new(),
            signal: None,
            events_outbox: Vec::new(),
            events_inflight: false,
            events_enabled: gateway,
            sync: RoomState::new(gateway),
            whoami: None,
            autoplay: AutoplayState::new(),
            settings: SettingsState::default(),
            server_url: None,
            exit_message: None,
        }
    }

    /// Apply the on-disk `[tui.autoplay]` config once at startup: seed the
    /// drift params and the initial on/off state.
    pub(crate) fn configure_autoplay(&mut self, cfg: &crate::config::AutoplayConfig) {
        self.autoplay.min_upcoming = cfg.min_upcoming;
        self.autoplay.enabled = cfg.enabled;
    }

    /// Seed the Settings working copy from the loaded config (quality,
    /// budgets, drift params) and record the server URL for the account card.
    pub(crate) fn configure_settings(&mut self, config: &crate::config::Config) {
        let ap = &config.tui.autoplay;
        self.settings.stream_quality = config.playback.stream_quality;
        self.settings.download_quality = config.playback.download_quality;
        self.settings.regular_budget_bytes = config.cache.regular_budget_bytes;
        self.settings.pinned_budget_bytes = config.cache.pinned_budget_bytes;
        self.settings.leash_tau = ap.leash_tau;
        self.settings.leash_lambda = ap.leash_lambda;
        self.settings.frontier_weight = ap.frontier_weight;
        self.settings.frontier_decay = ap.frontier_decay;
        self.settings.frontier_window = ap.frontier_window;
        self.settings.mmr_lambda = ap.mmr_lambda;
        self.server_url = Some(
            config
                .gateway
                .as_ref()
                .map_or_else(|| config.server.url.clone(), |g| g.url.clone()),
        );
    }

    /// Whether the calling principal is an admin (drives admin-only Settings
    /// rows). Unknown identity (direct mode / pre-fetch) is treated as non-admin.
    pub(crate) fn is_admin(&self) -> bool {
        self.whoami.as_ref().is_some_and(|w| w.role == "admin")
    }

    /// The interactive Settings rows in display order (shared by the reducer
    /// and the view so the cursor and the rendered list can't drift). The
    /// admin cache row is appended only for admins.
    pub(crate) fn settings_rows(&self) -> Vec<SettingRow> {
        let mut r = vec![
            SettingRow::StreamQuality,
            SettingRow::DownloadQuality,
            SettingRow::OutputDevice,
            SettingRow::RegularBudget,
            SettingRow::PinnedBudget,
            SettingRow::AutoplayEnabled,
            SettingRow::MinUpcoming,
            SettingRow::LeashTau,
            SettingRow::LeashLambda,
            SettingRow::FrontierWeight,
            SettingRow::FrontierDecay,
            SettingRow::FrontierWindow,
            SettingRow::MmrLambda,
            SettingRow::ResetAutoplay,
            SettingRow::SignOut,
        ];
        if self.is_admin() {
            r.push(SettingRow::InvalidateCache);
        }
        r
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

    pub(crate) fn player_resume(&self) {
        if let Some(p) = &self.player {
            p.resume();
        }
    }

    pub(crate) fn player_pause(&self) {
        if let Some(p) = &self.player {
            p.pause();
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
        artist_id: t.artist_id.as_ref().map(|a| a.as_str().to_owned()),
        album_id: t.album_id.as_ref().map(|a| a.as_str().to_owned()),
        duration: t.duration_seconds.map(u64::from).map(Duration::from_secs),
    }
}
