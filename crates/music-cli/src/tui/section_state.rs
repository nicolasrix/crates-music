//! Per-section view-state structs (Search, Stations, Liked, Downloads,
//! Settings). Split out of `state.rs` to keep that file focused — these are
//! self-contained data holders (their reducers live under `update::`) and are
//! re-exported from `state` so callers still say `state::SettingsState`.

use music_cache::AudioCacheStats;
use music_core::Track;
use music_subsonic::SearchResult3;
use ratatui::widgets::TableState;

use super::state::{LikedEntry, Loadable};
use super::widgets::input::InputField;

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
/// `InvalidateCache` is appended for admins alone (see `App::settings_rows`).
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
/// `enabled` / `min_upcoming` stay authoritative on `AutoplayState` and
/// output-on-device on `RoomState::output_on`, so the rows read/write those
/// directly. Every edit persists to the config file and applies live — see
/// `update::settings`. Seeded from disk by `App::configure_settings`.
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
