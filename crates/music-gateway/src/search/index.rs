//! In-memory fuzzy search index over the catalog, queried with `fst`
//! Levenshtein automata for typo tolerance.
//!
//! Build once (at boot / on refresh) from a flat list of [`Record`]s.
//! Query normalizes the input, and for each query token runs a
//! length-scaled Levenshtein automaton (with a prefix combinator, so a
//! half-typed word still matches) against a sorted term FST. Matches are
//! scored by field weight × match tier and aggregated per record, with a
//! precision-first AND over query tokens and an OR fallback for recall.
//!
//! The FST maps each distinct normalized *term* to an id into a postings
//! table; a posting says "this term appears in record N's field with
//! weight W". `fst::Map` needs unique, lexicographically-sorted keys, so
//! terms are gathered into a `BTreeMap` before the builder runs.

use std::collections::{BTreeMap, HashMap};

use fst::automaton::{Automaton, Levenshtein};
use fst::{IntoStreamer, Map, MapBuilder, Streamer};

use super::normalize::{normalize, tokenize};

/// Which catalog bucket a record belongs to. Mirrors Subsonic's
/// `search3` split so results map cleanly onto the response shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Artist,
    Album,
    Track,
}

/// One searchable catalog entity. `name` is the primary field (artist
/// name / album title / track title); `artist` is the secondary field
/// present on albums and tracks. The remaining fields ride along so the
/// handler can build a `search3`-shaped response without a second lookup.
#[derive(Debug, Clone)]
pub struct Record {
    pub kind: Kind,
    pub id: String,
    pub name: String,
    pub artist: Option<String>,
    pub artist_id: Option<String>,
    pub album: Option<String>,
    pub album_id: Option<String>,
    pub cover_art: Option<String>,
}

/// A single scored match, referencing back into the index's record table.
#[derive(Debug, Clone)]
pub struct Hit {
    pub record_index: usize,
    pub score: f32,
}

// Field weights — a title hit outranks an album hit outranks an artist
// hit, matching `searchRanking.ts`. The primary field always weighs 1.0;
// the secondary (artist) field is a supporting signal.
const SECONDARY_FIELD_WEIGHT: f32 = 0.4;

// Match tiers for a query token against an indexed term it reached.
const TIER_EXACT: f32 = 3.0; // term == token
const TIER_PREFIX: f32 = 2.0; // term starts with token (Navidrome-equivalent)
const TIER_FUZZY: f32 = 1.0; // within edit distance only

// Bonuses layered on top of per-token contributions.
const COVERAGE_BONUS: f32 = 5.0; // record matched *every* query token (AND)
const WHOLE_EXACT_BONUS: f32 = 20.0; // normalized full name == full query
const WHOLE_PREFIX_BONUS: f32 = 8.0; // normalized full name starts with query

/// A posting: term T occurs in `record_index`'s field of `weight`.
#[derive(Clone, Copy)]
struct Posting {
    record_index: usize,
    weight: f32,
}

/// Built, queryable index. Cheap to hold for ~10⁴ records.
pub struct SearchIndex {
    records: Vec<Record>,
    /// Normalized primary name per record, cached for the whole-name
    /// exact/prefix bonus (parallel to `records`).
    norm_names: Vec<String>,
    /// term → postings-group id (into `postings`).
    terms: Map<Vec<u8>>,
    postings: Vec<Vec<Posting>>,
}

// `fst::Map` isn't `Debug`; a record/term count is all any log line wants.
impl std::fmt::Debug for SearchIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchIndex")
            .field("records", &self.records.len())
            .field("terms", &self.postings.len())
            .finish_non_exhaustive()
    }
}

impl SearchIndex {
    /// Number of indexed records.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn record(&self, index: usize) -> Option<&Record> {
        self.records.get(index)
    }

    /// Build the index from a flat record list. Fails only if the FST
    /// builder rejects the (internally sorted, unique) term set — which
    /// shouldn't happen, but the error is surfaced rather than panicking.
    pub fn build(records: Vec<Record>) -> Result<Self, fst::Error> {
        let mut norm_names = Vec::with_capacity(records.len());
        // term → postings, gathered sorted+unique for the FST builder.
        let mut term_postings: BTreeMap<String, Vec<Posting>> = BTreeMap::new();

        for (idx, rec) in records.iter().enumerate() {
            let norm = normalize(&rec.name);
            for tok in norm.split(' ').filter(|t| !t.is_empty()) {
                term_postings
                    .entry(tok.to_string())
                    .or_default()
                    .push(Posting { record_index: idx, weight: 1.0 });
            }
            norm_names.push(norm);
            // Secondary field (album/track artist) as a supporting signal.
            if let Some(artist) = &rec.artist {
                for tok in tokenize(artist) {
                    term_postings.entry(tok).or_default().push(Posting {
                        record_index: idx,
                        weight: SECONDARY_FIELD_WEIGHT,
                    });
                }
            }
        }

        let mut postings: Vec<Vec<Posting>> = Vec::with_capacity(term_postings.len());
        let mut builder = MapBuilder::memory();
        for (term, group) in term_postings {
            let id = postings.len() as u64;
            postings.push(group);
            builder.insert(term.as_bytes(), id)?;
        }
        let terms = Map::new(builder.into_inner()?)?;

        Ok(Self { records, norm_names, terms, postings })
    }

    /// Rank records against `query`, returning up to `limit` hits of the
    /// given `kind`, best first. Empty when the query has no usable
    /// tokens or nothing matches.
    pub fn query(&self, query: &str, kind: Kind, limit: usize) -> Vec<Hit> {
        if limit == 0 {
            return Vec::new();
        }
        let q_tokens = tokenize(query);
        if q_tokens.is_empty() {
            return Vec::new();
        }
        let q_norm = normalize(query);

        // per record: best contribution seen for each query-token slot.
        // Using a slot vector (len = q_tokens) lets us both sum scores and
        // count coverage (how many distinct query tokens the record hit).
        let mut acc: HashMap<usize, Vec<f32>> = HashMap::new();

        for (ti, token) in q_tokens.iter().enumerate() {
            self.collect_token(token, ti, q_tokens.len(), &mut acc);
        }

        let mut hits: Vec<Hit> = Vec::new();
        let mut all_tokens_hits: Vec<Hit> = Vec::new();
        for (&rec_idx, slots) in &acc {
            let Some(rec) = self.records.get(rec_idx) else { continue };
            if rec.kind != kind {
                continue;
            }
            let matched = slots.iter().filter(|s| **s > 0.0).count();
            if matched == 0 {
                continue;
            }
            let mut score: f32 = slots.iter().sum();
            let covers_all = matched == q_tokens.len();
            if covers_all {
                score += COVERAGE_BONUS;
            }
            // Whole-name exact/prefix bonus — this is what floats "Led
            // Zeppelin" the artist above a track that merely mentions it.
            let norm = &self.norm_names[rec_idx];
            if *norm == q_norm {
                score += WHOLE_EXACT_BONUS;
            } else if norm.starts_with(&q_norm) {
                score += WHOLE_PREFIX_BONUS;
            }
            let hit = Hit { record_index: rec_idx, score };
            if covers_all {
                all_tokens_hits.push(hit.clone());
            }
            hits.push(hit);
        }

        // Precision first: if any record matched every query token, return
        // only those (AND). Otherwise fall back to partial matches (OR) so
        // a single unrecoverable token doesn't produce an empty page.
        let mut chosen = if all_tokens_hits.is_empty() { hits } else { all_tokens_hits };
        chosen.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                // Stable tiebreak by record index so equal scores are
                // deterministic across runs.
                .then(a.record_index.cmp(&b.record_index))
        });
        chosen.truncate(limit);
        chosen
    }

    /// Run one query token's Levenshtein automaton over the term FST and
    /// fold every matched term's postings into `acc`.
    fn collect_token(
        &self,
        token: &str,
        token_slot: usize,
        token_count: usize,
        acc: &mut HashMap<usize, Vec<f32>>,
    ) {
        let dist = edit_distance_for(token);
        // `.starts_with()` lets a partially-typed token match longer terms
        // within edit distance (so "zepp" reaches "zeppelin"). If the
        // automaton can't be built (pathological input) skip this token
        // rather than failing the whole query.
        let Ok(lev) = Levenshtein::new(token, dist) else {
            return;
        };
        let auto = lev.starts_with();
        let mut stream = self.terms.search(auto).into_stream();
        while let Some((term_bytes, group_id)) = stream.next() {
            let term = String::from_utf8_lossy(term_bytes);
            let tier = if term == *token {
                TIER_EXACT
            } else if term.starts_with(token) {
                TIER_PREFIX
            } else {
                TIER_FUZZY
            };
            let Ok(group_idx) = usize::try_from(group_id) else { continue };
            for posting in &self.postings[group_idx] {
                let contribution = tier * posting.weight;
                let slots = acc
                    .entry(posting.record_index)
                    .or_insert_with(|| vec![0.0; token_count]);
                if contribution > slots[token_slot] {
                    slots[token_slot] = contribution;
                }
            }
        }
    }
}

/// Edit-distance budget scaled by token length — the standard SymSpell-ish
/// policy. Short tokens get no slack (else "the" fuzzes into everything);
/// long tokens tolerate two errors.
fn edit_distance_for(token: &str) -> u32 {
    match token.chars().count() {
        0..=3 => 0,
        4..=7 => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artist(id: &str, name: &str) -> Record {
        Record {
            kind: Kind::Artist,
            id: id.into(),
            name: name.into(),
            artist: None,
            artist_id: None,
            album: None,
            album_id: None,
            cover_art: None,
        }
    }

    fn track(id: &str, title: &str, artist_name: &str) -> Record {
        Record {
            kind: Kind::Track,
            id: id.into(),
            name: title.into(),
            artist: Some(artist_name.into()),
            artist_id: Some("ar-x".into()),
            album: None,
            album_id: None,
            cover_art: None,
        }
    }

    fn index() -> SearchIndex {
        SearchIndex::build(vec![
            artist("ar-1", "Led Zeppelin"),
            artist("ar-2", "Radiohead"),
            artist("ar-3", "The Rolling Stones"),
            track("tr-1", "Stairway to Heaven", "Led Zeppelin"),
            track("tr-2", "Paranoid Android", "Radiohead"),
        ])
        .unwrap()
    }

    fn top_id<'a>(idx: &'a SearchIndex, hits: &[Hit]) -> Option<&'a str> {
        hits.first().map(|h| idx.record(h.record_index).unwrap().id.as_str())
    }

    #[test]
    fn exact_match_ranks_first() {
        let idx = index();
        let hits = idx.query("led zeppelin", Kind::Artist, 10);
        assert_eq!(top_id(&idx, &hits), Some("ar-1"));
    }

    #[test]
    fn tolerates_a_typo() {
        // "zeplin" — a dropped 'p', and a substituted vowel below — are
        // exactly what Navidrome's prefix match fails on.
        let idx = index();
        let hits = idx.query("led zeplin", Kind::Artist, 10);
        assert_eq!(top_id(&idx, &hits), Some("ar-1"), "led zeplin → Led Zeppelin");

        let hits2 = idx.query("radiohed", Kind::Artist, 10);
        assert_eq!(top_id(&idx, &hits2), Some("ar-2"), "radiohed → Radiohead");
    }

    #[test]
    fn partial_token_matches_via_prefix() {
        let idx = index();
        let hits = idx.query("zepp", Kind::Artist, 10);
        assert_eq!(top_id(&idx, &hits), Some("ar-1"));
    }

    #[test]
    fn short_tokens_do_not_fuzz_into_everything() {
        // "the" (≤3 chars) gets zero edit budget, so it must not fuzzy-hit
        // "led"/other 3-letter tokens — only the literal "The Rolling
        // Stones" article.
        let idx = index();
        let hits = idx.query("the", Kind::Artist, 10);
        let ids: Vec<&str> =
            hits.iter().map(|h| idx.record(h.record_index).unwrap().id.as_str()).collect();
        assert_eq!(ids, vec!["ar-3"]);
    }

    #[test]
    fn secondary_artist_field_surfaces_tracks() {
        // Query the artist name; the track by that artist should be found
        // via its secondary (artist) field even though its title doesn't
        // contain the query.
        let idx = index();
        let hits = idx.query("led zeppelin", Kind::Track, 10);
        assert_eq!(top_id(&idx, &hits), Some("tr-1"));
    }

    #[test]
    fn kind_filter_isolates_buckets() {
        let idx = index();
        assert!(idx.query("led zeppelin", Kind::Album, 10).is_empty());
    }

    #[test]
    fn no_query_no_hits() {
        let idx = index();
        assert!(idx.query("   ", Kind::Artist, 10).is_empty());
        assert!(idx.query("led", Kind::Artist, 0).is_empty());
    }
}
