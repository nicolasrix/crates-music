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

// ── diagnostics (section 9, admin-only) ─────────────────────────────────────

use crate::api::{
    ClientEvent, HistogramBucket, LatentNeighbour, LatentSpace, QueueDepth, RecentlyPlayed,
    RecommenderPanels, TraceEntry,
};

/// The Diagnostics sub-tabs, cycled with `h`/`l` (the section-kind pattern
/// Search uses for its buckets). One per family of gateway inspector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DiagTab {
    #[default]
    Ingest,
    Recommender,
    Listening,
    Tracing,
    LatentSpace,
    ClientEvents,
}

impl DiagTab {
    pub(crate) const ALL: [Self; 6] = [
        Self::Ingest,
        Self::Recommender,
        Self::Listening,
        Self::Tracing,
        Self::LatentSpace,
        Self::ClientEvents,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Ingest => "Ingest",
            Self::Recommender => "Recommender",
            Self::Listening => "Listening",
            Self::Tracing => "Tracing",
            Self::LatentSpace => "Latent space",
            Self::ClientEvents => "Client events",
        }
    }
}

/// Time window for the inspectors that accept a `since_ms` lower bound
/// (Recommender panels + Tracing histogram). Cycled with `[`/`]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DiagWindow {
    M15,
    H1,
    #[default]
    H24,
    All,
}

impl DiagWindow {
    pub(crate) const ALL: [Self; 4] = [Self::M15, Self::H1, Self::H24, Self::All];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::M15 => "15m",
            Self::H1 => "1h",
            Self::H24 => "24h",
            Self::All => "all",
        }
    }

    /// The `since_ms` lower bound for a given `now` (unix-ms), or `None` for
    /// "all time" (the query param is omitted).
    pub(crate) fn cutoff_ms(self, now_ms: i64) -> Option<i64> {
        let span_ms: i64 = match self {
            Self::M15 => 15 * 60_000,
            Self::H1 => 60 * 60_000,
            Self::H24 => 24 * 60 * 60_000,
            Self::All => return None,
        };
        Some(now_ms - span_ms)
    }

    pub(crate) fn index(self) -> usize {
        Self::ALL.iter().position(|w| *w == self).unwrap_or(0)
    }
}

/// The Tracing sub-tab's two datasets (fetched together for one window).
#[derive(Debug, Clone)]
pub(crate) struct TracingData {
    pub traces: Vec<TraceEntry>,
    pub histogram: Vec<HistogramBucket>,
}

/// Section 9 — admin-only diagnostics. Each sub-tab holds its own `Loadable`
/// so switching tabs shows the last data instantly while the fresh fetch
/// runs; the active tab auto-refreshes on the 250 ms tick, throttled to ~5 s
/// (mirrors the web `/diagnostics` `refetchInterval`).
#[derive(Debug, Default)]
pub(crate) struct DiagnosticsState {
    /// Active sub-tab (index into [`DiagTab::ALL`]).
    pub tab: usize,
    /// Window for the `since_ms` inspectors.
    pub window: DiagWindow,
    /// Tick of the last (auto or on-enter) refresh of the active tab.
    pub last_refresh_tick: u64,

    pub ingest: Loadable<QueueDepth>,
    pub recommender: Loadable<RecommenderPanels>,
    pub listening: Loadable<Vec<RecentlyPlayed>>,
    pub listening_table: TableState,
    pub tracing: Loadable<TracingData>,
    pub tracing_table: TableState,
    pub latent: Loadable<LatentSpace>,
    /// Cursor into `latent.points` (the highlighted scatter point).
    pub latent_table: TableState,
    /// Nearest-neighbour side list for the selected latent point.
    pub neighbours: Loadable<Vec<LatentNeighbour>>,
    /// The point id `neighbours` was fetched for (dedups the tick-driven
    /// re-fetch: only re-request when the selection actually moved).
    pub neighbour_seed: Option<String>,
    pub client_events: Loadable<Vec<ClientEvent>>,
    pub client_events_table: TableState,
    /// Unused stand-in returned for the non-navigable tabs (Ingest,
    /// Recommender) so `focused_list` always has a table to hand back and
    /// cursor keys no-op without disturbing a real selection.
    pub scratch_table: TableState,
}

impl DiagnosticsState {
    pub(crate) fn active_tab(&self) -> DiagTab {
        DiagTab::ALL[self.tab % DiagTab::ALL.len()]
    }
}
