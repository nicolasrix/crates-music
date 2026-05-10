//! Queue-aware diversity filter for recommend handlers.
//!
//! Given a queue snapshot and a stream of recommendation candidates,
//! enforces:
//!
//! 1. **Don't recommend tracks already in the queue.** Exact `track_id`
//!    match is the cheapest filter and catches re-additions.
//! 2. **Per-artist cap.** No more than `max_per_artist` tracks by the
//!    same artist in the queue at any time. The currently-playing track
//!    *counts* toward the cap (so it constrains future suggestions) but
//!    is not itself excluded from results — it might still appear as
//!    its own recommendation seed in odd cases, and the rule the user
//!    cares about is "the artist's footprint in the *upcoming* queue".
//! 3. **Title dedup.** `(artist_key, title_normalized)` collapses
//!    cross-edition duplicates (album version vs single, remaster vs
//!    original) so the queue doesn't double up on essentially the same
//!    song.
//!
//! All three rules are queue-aware: the filter mutates as candidates
//! are accepted, so a recommendation accepted *this* refill counts
//! against the cap for the next candidate in the same call.
//!
//! ## Why a pure module
//!
//! Wire shape (queue ids + knobs) is decided in the HTTP layer, but the
//! decision logic is the same for every endpoint that takes
//! `queue_context`. Keeping it pure here makes it trivially unit-
//! testable and keeps the handlers focused on payload validation +
//! ANN/aggregation + serialization.

use std::collections::{HashMap, HashSet};

use music_core::TrackId;

use crate::metadata::TrackMetadata;

/// Outcome of [`QueueFilter::try_accept`]. Carries enough detail for
/// the handlers to split rejection counts by reason in the trace
/// store (artist cap vs cross-edition dedup), so /diagnostics can
/// show which knob is doing the work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterDecision {
    /// Candidate survived all checks and was admitted into the slate.
    /// Internal counts (artist + dedup) are now bumped.
    Accept,
    /// Candidate was rejected because the artist's queue footprint is
    /// at the per-artist cap. State unchanged.
    RejectArtistCap,
    /// Candidate was rejected because `(artist_key, title_normalized)`
    /// matches an entry already in the queue or already accepted in
    /// this call. State unchanged.
    RejectDedup,
}

impl FilterDecision {
    /// Convenience for the common boolean question "was this admitted?".
    /// Tests and call sites that only care about the binary outcome
    /// should use this; sites that record diagnostics should match on
    /// the full enum.
    pub fn is_accept(self) -> bool {
        matches!(self, Self::Accept)
    }
}

/// How the slate is selected from the over-fetched ANN candidate pool.
///
/// - [`Self::HardCap`] — current/legacy behaviour. The candidate stream
///   is walked in ANN-similarity order; each survivor is admitted unless
///   the per-artist cap or title dedup fires. **Default.**
/// - [`Self::Mmr`] — Maximal Marginal Relevance re-ranks the post-
///   exclusion candidate pool, balancing relevance against diversity
///   via [`QueueFilterConfig::mmr_lambda`]. The artist cap and title
///   dedup still run downstream as a safety net.
/// - [`Self::Off`] — admit candidates in input order until `top_n` is
///   reached, with no diversity gating beyond the queue exclusion list.
///   Useful for A/B comparisons; not recommended for everyday use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiversityMode {
    #[default]
    HardCap,
    Mmr,
    Off,
}

/// Knobs controlling filter strictness. Sensible defaults match the
/// web client's old `MAX_PER_ARTIST = 2` + `dedupeKey` behavior so
/// switching to server-side filtering is behavior-preserving.
#[derive(Clone, Copy, Debug)]
pub struct QueueFilterConfig {
    /// Selection algorithm for the slate. See [`DiversityMode`].
    pub diversity_mode: DiversityMode,
    /// MMR relevance/novelty tradeoff in `[0, 1]`. Only consulted when
    /// `diversity_mode == DiversityMode::Mmr`. `1.0` = pure relevance
    /// (equivalent to `Off`); `0.0` = pure novelty after the
    /// relevance-driven first pick.
    pub mmr_lambda: f32,
    /// Max tracks per artist key in the resulting queue. `0` disables
    /// the cap entirely (useful for test stations / one-artist
    /// playlists).
    ///
    /// Under [`DiversityMode::HardCap`] this is the primary diversity
    /// mechanism. Under [`DiversityMode::Mmr`] it acts as a safety net
    /// — MMR shapes the slate softly, but an extreme λ (or a degenerate
    /// embedding distribution) shouldn't produce 8 tracks by the same
    /// artist. Set to `0` to make MMR the sole diversity controller.
    pub max_per_artist: u32,
    /// When `true`, drop candidates whose `(artist_key,
    /// title_normalized)` is already present in the queue or already
    /// accepted earlier in this call. Runs orthogonally to the
    /// diversity mode — title dedup answers a different question (cross-
    /// edition redundancy) than per-artist diversity.
    pub dedup_titles: bool,
}

impl Default for QueueFilterConfig {
    fn default() -> Self {
        Self {
            diversity_mode: DiversityMode::HardCap,
            mmr_lambda: 0.7,
            max_per_artist: 2,
            dedup_titles: true,
        }
    }
}

/// Stateful filter: builds queue-derived counts, then mutates as
/// candidates are accepted.
#[derive(Debug)]
pub struct QueueFilter {
    cfg: QueueFilterConfig,
    /// Tracks excluded from results outright (already in the queue,
    /// minus the now-playing track). The now-playing track is omitted
    /// here on purpose — see module doc.
    excluded: HashSet<TrackId>,
    /// Per-artist count, including queue items + already-accepted
    /// candidates in the current call.
    artist_counts: HashMap<String, u32>,
    /// `(artist_key, title_normalized)` set, same lifecycle as
    /// `artist_counts`.
    dedup_keys: HashSet<String>,
}

impl QueueFilter {
    /// Build from queue track ids + a metadata lookup. `metadata`
    /// should typically be the result of [`MetadataStore::get_many`]
    /// over `queue_track_ids`. Tracks missing from `metadata` count
    /// toward exclusion (we still drop them from results) but not
    /// toward `artist_counts` / `dedup_keys` — we don't know their
    /// artist or title, so we can't constrain on those axes.
    ///
    /// `now_playing` is included in the artist/dedup state but not in
    /// `excluded`, exactly as the user requested: "excluding the
    /// currently playing song" from the *exclusion* set.
    pub fn build(
        queue_track_ids: &[TrackId],
        now_playing: Option<&TrackId>,
        metadata: &HashMap<TrackId, TrackMetadata>,
        cfg: QueueFilterConfig,
    ) -> Self {
        let mut excluded = HashSet::with_capacity(queue_track_ids.len());
        let mut artist_counts: HashMap<String, u32> = HashMap::new();
        let mut dedup_keys: HashSet<String> = HashSet::new();

        for id in queue_track_ids {
            if Some(id) != now_playing {
                excluded.insert(id.clone());
            }
            if let Some(m) = metadata.get(id) {
                let ak = artist_key(m);
                *artist_counts.entry(ak.clone()).or_insert(0) += 1;
                if cfg.dedup_titles {
                    dedup_keys.insert(format!("{ak}|{}", m.title_normalized));
                }
            }
        }

        Self {
            cfg,
            excluded,
            artist_counts,
            dedup_keys,
        }
    }

    /// True if this candidate is in the queue (and so already known to
    /// the user's player). Cheap pre-check before metadata lookup.
    pub fn is_excluded(&self, id: &TrackId) -> bool {
        self.excluded.contains(id)
    }

    /// Try to accept a candidate. Returns [`FilterDecision::Accept`] if
    /// it survives all filters (state mutated to reflect the accept),
    /// otherwise the specific [`FilterDecision`] variant that fired
    /// (state unchanged).
    ///
    /// Candidates without metadata in the cache pass the artist-cap
    /// and dedup checks (we have no signal to constrain them with),
    /// but are still subject to `is_excluded`. The caller should still
    /// run [`Self::is_excluded`] first — `try_accept` does *not*
    /// re-check exclusion, since handlers benefit from a span-friendly
    /// breakdown of exclusion vs. cap-rejection.
    pub fn try_accept(&mut self, meta: Option<&TrackMetadata>) -> FilterDecision {
        let Some(m) = meta else {
            // No metadata → we have no artist or title to gate on.
            // Pass through; a follow-up backfill will eventually fill
            // the row in and the next refill will gate on it.
            return FilterDecision::Accept;
        };
        let ak = artist_key(m);
        if self.cfg.max_per_artist > 0
            && self.artist_counts.get(&ak).copied().unwrap_or(0) >= self.cfg.max_per_artist
        {
            return FilterDecision::RejectArtistCap;
        }
        let dk = format!("{ak}|{}", m.title_normalized);
        if self.cfg.dedup_titles && self.dedup_keys.contains(&dk) {
            return FilterDecision::RejectDedup;
        }
        // Accept: bump counts.
        *self.artist_counts.entry(ak).or_insert(0) += 1;
        if self.cfg.dedup_titles {
            self.dedup_keys.insert(dk);
        }
        FilterDecision::Accept
    }
}

/// Key under which we count "same artist". Prefer the stable id; fall
/// back to lowercased name when the cache row was built from a source
/// that didn't carry an id (rare today, possible in the future).
fn artist_key(m: &TrackMetadata) -> String {
    if let Some(id) = m.artist_id.as_deref()
        && !id.is_empty()
    {
        return format!("id:{id}");
    }
    let name = m.artist.trim().to_lowercase();
    if name.is_empty() {
        // Empty artist is its own bucket — better than crashing or
        // lumping unrelated tracks together.
        return "name:".to_string();
    }
    format!("name:{name}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::normalize_title;

    fn track(id: &str, artist_id: Option<&str>, artist: &str, title: &str) -> TrackMetadata {
        TrackMetadata {
            track_id: TrackId::from(id),
            artist_id: artist_id.map(String::from),
            artist: artist.into(),
            album_id: None,
            album: None,
            title: title.into(),
            title_normalized: normalize_title(title),
            duration_seconds: None,
            genre: None,
            year: None,
            track_number: None,
            disc_number: None,
            bpm: None,
            musical_key: None,
        }
    }

    fn meta_map(ts: &[TrackMetadata]) -> HashMap<TrackId, TrackMetadata> {
        ts.iter().map(|t| (t.track_id.clone(), t.clone())).collect()
    }

    fn ids(slice: &[&str]) -> Vec<TrackId> {
        slice.iter().map(|s| TrackId::from(*s)).collect()
    }

    // --- exclusion ---

    #[test]
    fn excludes_queue_tracks() {
        let queue = ids(&["q1", "q2"]);
        let f = QueueFilter::build(&queue, None, &HashMap::new(), QueueFilterConfig::default());
        assert!(f.is_excluded(&TrackId::from("q1")));
        assert!(f.is_excluded(&TrackId::from("q2")));
        assert!(!f.is_excluded(&TrackId::from("q3")));
    }

    #[test]
    fn now_playing_is_not_in_exclusion_set() {
        let queue = ids(&["q1", "now", "q3"]);
        let now = TrackId::from("now");
        let f = QueueFilter::build(
            &queue,
            Some(&now),
            &HashMap::new(),
            QueueFilterConfig::default(),
        );
        assert!(f.is_excluded(&TrackId::from("q1")));
        assert!(!f.is_excluded(&now));
        assert!(f.is_excluded(&TrackId::from("q3")));
    }

    #[test]
    fn empty_queue_excludes_nothing() {
        let f = QueueFilter::build(&[], None, &HashMap::new(), QueueFilterConfig::default());
        assert!(!f.is_excluded(&TrackId::from("anything")));
    }

    // --- artist cap ---

    #[test]
    fn artist_cap_blocks_after_cap_reached() {
        let q1 = track("q1", Some("ar1"), "Queen", "Bohemian Rhapsody");
        let q2 = track("q2", Some("ar1"), "Queen", "Killer Queen");
        let metadata = meta_map(&[q1.clone(), q2.clone()]);
        let queue = ids(&["q1", "q2"]);
        let mut f = QueueFilter::build(
            &queue,
            None,
            &metadata,
            QueueFilterConfig {
                max_per_artist: 2,
                dedup_titles: false,
                ..QueueFilterConfig::default()
            },
        );
        // ar1 is already at cap (2). Any new ar1 candidate is rejected.
        let cand = track("c1", Some("ar1"), "Queen", "Don't Stop Me Now");
        assert!(!f.try_accept(Some(&cand)).is_accept());
    }

    #[test]
    fn artist_cap_admits_under_cap() {
        let q1 = track("q1", Some("ar1"), "Queen", "Bohemian Rhapsody");
        let metadata = meta_map(std::slice::from_ref(&q1));
        let queue = ids(&["q1"]);
        let mut f = QueueFilter::build(
            &queue,
            None,
            &metadata,
            QueueFilterConfig {
                max_per_artist: 2,
                dedup_titles: false,
                ..QueueFilterConfig::default()
            },
        );
        let cand = track("c1", Some("ar1"), "Queen", "Killer Queen");
        assert!(f.try_accept(Some(&cand)).is_accept());
        // Now at cap; next ar1 rejected.
        let cand2 = track("c2", Some("ar1"), "Queen", "Don't Stop Me Now");
        assert!(!f.try_accept(Some(&cand2)).is_accept());
    }

    #[test]
    fn now_playing_counts_toward_artist_cap() {
        let now_meta = track("now", Some("ar1"), "Queen", "Bohemian Rhapsody");
        let q2 = track("q2", Some("ar1"), "Queen", "Killer Queen");
        let metadata = meta_map(&[now_meta.clone(), q2.clone()]);
        let queue = ids(&["now", "q2"]);
        let now = TrackId::from("now");
        let mut f = QueueFilter::build(
            &queue,
            Some(&now),
            &metadata,
            QueueFilterConfig {
                max_per_artist: 2,
                dedup_titles: false,
                ..QueueFilterConfig::default()
            },
        );
        // ar1 has 2 entries (now-playing + q2) → at cap.
        let cand = track("c1", Some("ar1"), "Queen", "Don't Stop Me Now");
        assert!(!f.try_accept(Some(&cand)).is_accept());
    }

    #[test]
    fn artist_cap_zero_disables() {
        let q1 = track("q1", Some("ar1"), "Queen", "Bohemian Rhapsody");
        let q2 = track("q2", Some("ar1"), "Queen", "Killer Queen");
        let metadata = meta_map(&[q1.clone(), q2.clone()]);
        let queue = ids(&["q1", "q2"]);
        let mut f = QueueFilter::build(
            &queue,
            None,
            &metadata,
            QueueFilterConfig {
                max_per_artist: 0,
                dedup_titles: false,
                ..QueueFilterConfig::default()
            },
        );
        let cand = track("c1", Some("ar1"), "Queen", "Don't Stop Me Now");
        assert!(f.try_accept(Some(&cand)).is_accept());
    }

    #[test]
    fn artist_cap_falls_back_to_name_when_no_id() {
        let q1 = track("q1", None, "Queen", "Bohemian Rhapsody");
        let q2 = track("q2", None, "queen", "Killer Queen"); // case-fold
        let metadata = meta_map(&[q1.clone(), q2.clone()]);
        let queue = ids(&["q1", "q2"]);
        let mut f = QueueFilter::build(
            &queue,
            None,
            &metadata,
            QueueFilterConfig {
                max_per_artist: 2,
                dedup_titles: false,
                ..QueueFilterConfig::default()
            },
        );
        let cand = track("c1", None, "QUEEN", "Don't Stop Me Now");
        assert!(!f.try_accept(Some(&cand)).is_accept());
    }

    #[test]
    fn accepted_candidate_counts_against_subsequent_candidates() {
        // Queue empty. Two candidates by the same artist; cap = 1. The
        // first candidate accepted should block the second.
        let mut f = QueueFilter::build(
            &[],
            None,
            &HashMap::new(),
            QueueFilterConfig {
                max_per_artist: 1,
                dedup_titles: false,
                ..QueueFilterConfig::default()
            },
        );
        let c1 = track("c1", Some("ar1"), "Queen", "Bohemian Rhapsody");
        let c2 = track("c2", Some("ar1"), "Queen", "Killer Queen");
        assert!(f.try_accept(Some(&c1)).is_accept());
        assert!(!f.try_accept(Some(&c2)).is_accept());
    }

    // --- title dedup ---

    #[test]
    fn title_dedup_blocks_remaster_when_original_is_queued() {
        let q1 = track("q1", Some("ar1"), "Queen", "Bohemian Rhapsody");
        let metadata = meta_map(std::slice::from_ref(&q1));
        let queue = ids(&["q1"]);
        let mut f = QueueFilter::build(
            &queue,
            None,
            &metadata,
            QueueFilterConfig {
                max_per_artist: 0, // disable artist cap to isolate dedup
                dedup_titles: true,
                ..QueueFilterConfig::default()
            },
        );
        let cand = track(
            "c1",
            Some("ar1"),
            "Queen",
            "Bohemian Rhapsody (Remastered 2011)",
        );
        assert!(!f.try_accept(Some(&cand)).is_accept());
    }

    #[test]
    fn title_dedup_allows_same_title_by_different_artist() {
        let q1 = track("q1", Some("ar1"), "Queen", "Crazy Little Thing");
        let metadata = meta_map(std::slice::from_ref(&q1));
        let queue = ids(&["q1"]);
        let mut f = QueueFilter::build(
            &queue,
            None,
            &metadata,
            QueueFilterConfig {
                max_per_artist: 0,
                dedup_titles: true,
                ..QueueFilterConfig::default()
            },
        );
        let cand = track("c1", Some("ar2"), "Other", "Crazy Little Thing");
        assert!(f.try_accept(Some(&cand)).is_accept());
    }

    #[test]
    fn title_dedup_disabled_admits_remasters() {
        let q1 = track("q1", Some("ar1"), "Queen", "Bohemian Rhapsody");
        let metadata = meta_map(std::slice::from_ref(&q1));
        let queue = ids(&["q1"]);
        let mut f = QueueFilter::build(
            &queue,
            None,
            &metadata,
            QueueFilterConfig {
                max_per_artist: 0,
                dedup_titles: false,
                ..QueueFilterConfig::default()
            },
        );
        let cand = track(
            "c1",
            Some("ar1"),
            "Queen",
            "Bohemian Rhapsody (Remastered 2011)",
        );
        assert!(f.try_accept(Some(&cand)).is_accept());
    }

    #[test]
    fn title_dedup_blocks_second_candidate_with_same_title() {
        // Queue empty; both candidates are the same song under
        // different track ids (album vs single). First accepted, second
        // rejected even though the queue itself didn't carry a dedup key.
        let mut f = QueueFilter::build(
            &[],
            None,
            &HashMap::new(),
            QueueFilterConfig {
                max_per_artist: 0,
                dedup_titles: true,
                ..QueueFilterConfig::default()
            },
        );
        let c1 = track("c1", Some("ar1"), "Queen", "Bohemian Rhapsody");
        let c2 = track("c2", Some("ar1"), "Queen", "Bohemian Rhapsody (Live)");
        assert!(f.try_accept(Some(&c1)).is_accept());
        assert!(!f.try_accept(Some(&c2)).is_accept());
    }

    // --- missing metadata ---

    #[test]
    fn candidate_without_metadata_passes_through() {
        // Cache miss → no signal to gate on. Pass through.
        let mut f = QueueFilter::build(&[], None, &HashMap::new(), QueueFilterConfig::default());
        assert!(f.try_accept(None).is_accept());
    }

    #[test]
    fn queue_track_without_metadata_still_excluded_but_does_not_count() {
        // q1 in queue, no metadata cached for it. ar1 candidate should
        // not be blocked (q1 doesn't contribute to artist count).
        let queue = ids(&["q1"]);
        let mut f = QueueFilter::build(
            &queue,
            None,
            &HashMap::new(),
            QueueFilterConfig {
                max_per_artist: 1,
                dedup_titles: false,
                ..QueueFilterConfig::default()
            },
        );
        assert!(f.is_excluded(&TrackId::from("q1")));
        let cand = track("c1", Some("ar1"), "Queen", "Killer Queen");
        assert!(f.try_accept(Some(&cand)).is_accept());
    }

    // --- decision-reason reporting ---

    #[test]
    fn try_accept_returns_accept_for_admitted_candidate() {
        let mut f = QueueFilter::build(&[], None, &HashMap::new(), QueueFilterConfig::default());
        let c = track("c1", Some("ar1"), "Queen", "Bohemian Rhapsody");
        assert_eq!(f.try_accept(Some(&c)), FilterDecision::Accept);
    }

    #[test]
    fn try_accept_returns_reject_artist_cap_when_at_cap() {
        let q1 = track("q1", Some("ar1"), "Queen", "Bohemian Rhapsody");
        let q2 = track("q2", Some("ar1"), "Queen", "Killer Queen");
        let metadata = meta_map(&[q1.clone(), q2.clone()]);
        let queue = ids(&["q1", "q2"]);
        let mut f = QueueFilter::build(
            &queue,
            None,
            &metadata,
            QueueFilterConfig {
                max_per_artist: 2,
                dedup_titles: false,
                ..QueueFilterConfig::default()
            },
        );
        let cand = track("c1", Some("ar1"), "Queen", "Don't Stop Me Now");
        assert_eq!(f.try_accept(Some(&cand)), FilterDecision::RejectArtistCap,);
    }

    #[test]
    fn try_accept_returns_reject_dedup_when_title_collides() {
        // Title dedup must be the firing reason, so disable artist cap
        // entirely — otherwise we couldn't tell which check fired first.
        let q1 = track("q1", Some("ar1"), "Queen", "Bohemian Rhapsody");
        let metadata = meta_map(std::slice::from_ref(&q1));
        let queue = ids(&["q1"]);
        let mut f = QueueFilter::build(
            &queue,
            None,
            &metadata,
            QueueFilterConfig {
                max_per_artist: 0,
                dedup_titles: true,
                ..QueueFilterConfig::default()
            },
        );
        let cand = track(
            "c1",
            Some("ar1"),
            "Queen",
            "Bohemian Rhapsody (Remastered 2011)",
        );
        assert_eq!(f.try_accept(Some(&cand)), FilterDecision::RejectDedup);
    }

    // --- empty/edge ---

    #[test]
    fn empty_queue_with_default_config_admits_first_candidate() {
        let mut f = QueueFilter::build(&[], None, &HashMap::new(), QueueFilterConfig::default());
        let c = track("c1", Some("ar1"), "Queen", "Bohemian Rhapsody");
        assert!(f.try_accept(Some(&c)).is_accept());
    }

    #[test]
    fn empty_artist_id_falls_back_to_name() {
        let m = track("c1", Some(""), "Queen", "Bohemian Rhapsody");
        // Should produce a `name:queen` key, not `id:`. Verify by
        // building a queue containing this track and checking that an
        // id-less candidate by the same name collides under cap=1.
        let metadata = meta_map(std::slice::from_ref(&m));
        let queue = ids(&["c1"]);
        let mut f = QueueFilter::build(
            &queue,
            None,
            &metadata,
            QueueFilterConfig {
                max_per_artist: 1,
                dedup_titles: false,
                ..QueueFilterConfig::default()
            },
        );
        let cand = track("c2", None, "Queen", "Killer Queen");
        assert!(!f.try_accept(Some(&cand)).is_accept());
    }
}
