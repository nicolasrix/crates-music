//! Tests for the cosine ANN index.
//!
//! Pin behavior, not internals: we don't care which neighbour comes
//! out at rank 3, only that the closest vector wins, that persistence
//! round-trips, and that the index handles the corner cases.

use music_core::TrackId;
use music_recommend::ann::{AnnIndex, AnnQueryResult};

const DIM: usize = 8;

fn unit_at(i: usize) -> Vec<f32> {
    let mut v = vec![0.0_f32; DIM];
    v[i] = 1.0;
    v
}

#[test]
fn empty_index_returns_no_neighbours() {
    let idx = AnnIndex::open_in_memory(DIM, 16).expect("open");
    let hits = idx.query(&unit_at(0), 5).expect("query");
    assert!(hits.is_empty());
}

#[test]
fn closest_basis_vector_is_top_hit() {
    let idx = AnnIndex::open_in_memory(DIM, 16).expect("open");
    for i in 0..DIM {
        idx.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .expect("upsert");
    }
    let hits = idx.query(&unit_at(3), 1).expect("query");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].track_id.as_str(), "t3");
    assert!(
        (hits[0].similarity - 1.0).abs() < 1e-5,
        "similarity for identical vectors should be 1.0, got {}",
        hits[0].similarity
    );
}

#[test]
fn top_k_returns_at_most_k() {
    let idx = AnnIndex::open_in_memory(DIM, 16).expect("open");
    for i in 0..DIM {
        idx.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .expect("upsert");
    }
    let hits = idx.query(&unit_at(0), 3).expect("query");
    assert!(hits.len() <= 3);
    assert_eq!(hits[0].track_id.as_str(), "t0");
}

#[test]
fn query_returns_cosine_similarity_in_descending_order() {
    let idx = AnnIndex::open_in_memory(DIM, 16).expect("open");
    for i in 0..DIM {
        idx.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .expect("upsert");
    }
    let hits = idx.query(&unit_at(0), DIM).expect("query");
    // First hit should have highest similarity.
    for w in hits.windows(2) {
        assert!(
            w[0].similarity >= w[1].similarity,
            "results not sorted: {hits:?}"
        );
    }
}

#[test]
fn upsert_replaces_existing_vector() {
    // Re-upserting the same track_id with a different vector must
    // not duplicate the entry — this is the "re-embedding under a new
    // model" path the ingest worker uses.
    let idx = AnnIndex::open_in_memory(DIM, 16).expect("open");
    let id = TrackId::from("t0");
    idx.upsert(&id, &unit_at(0)).expect("upsert v1");
    idx.upsert(&id, &unit_at(5)).expect("upsert v2");

    // Querying for unit_at(0) should not return t0 first now — it's
    // been moved to the e5 axis.
    let hits = idx.query(&unit_at(5), 1).expect("query");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].track_id.as_str(), "t0");
    assert!((hits[0].similarity - 1.0).abs() < 1e-5);

    // Index size should still be 1.
    assert_eq!(idx.len().expect("len"), 1);
}

#[test]
fn remove_evicts_track() {
    let idx = AnnIndex::open_in_memory(DIM, 16).expect("open");
    let id = TrackId::from("t0");
    idx.upsert(&id, &unit_at(0)).expect("upsert");
    assert_eq!(idx.len().expect("len"), 1);

    idx.remove(&id).expect("remove");
    assert_eq!(idx.len().expect("len"), 0);
    let hits = idx.query(&unit_at(0), 5).expect("query");
    assert!(hits.is_empty());
}

#[test]
fn rejects_wrong_dimension_on_upsert() {
    let idx = AnnIndex::open_in_memory(DIM, 16).expect("open");
    let bad = [0.0; DIM + 1];
    let err = idx.upsert(&TrackId::from("t0"), &bad);
    assert!(err.is_err(), "expected dim mismatch error");
}

#[test]
fn rejects_wrong_dimension_on_query() {
    let idx = AnnIndex::open_in_memory(DIM, 16).expect("open");
    let bad = [0.0; DIM + 1];
    let err = idx.query(&bad, 5);
    assert!(err.is_err(), "expected dim mismatch error");
}

#[test]
fn persistence_round_trip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ann.bin");

    {
        let idx = AnnIndex::open(&path, DIM, 16).expect("open");
        idx.upsert(&TrackId::from("alpha"), &unit_at(0))
            .expect("upsert");
        idx.upsert(&TrackId::from("beta"), &unit_at(1))
            .expect("upsert");
        idx.persist().expect("persist");
    }

    // Reopen.
    let idx = AnnIndex::open(&path, DIM, 16).expect("reopen");
    assert_eq!(idx.len().expect("len"), 2);

    let hits = idx.query(&unit_at(0), 1).expect("query");
    assert_eq!(hits[0].track_id.as_str(), "alpha");
}

#[test]
fn query_excludes_self_when_requested() {
    // Common pattern: "give me 5 tracks similar to seed X, but not X
    // itself". Index supports this via an exclude param so callers
    // don't have to over-fetch and filter.
    let idx = AnnIndex::open_in_memory(DIM, 16).expect("open");
    let seed = TrackId::from("seed");
    idx.upsert(&seed, &unit_at(0)).expect("upsert seed");
    for i in 1..DIM {
        idx.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .expect("upsert");
    }

    let hits = idx
        .query_excluding(&unit_at(0), 5, std::slice::from_ref(&seed))
        .expect("query");
    assert!(!hits.iter().any(|h| h.track_id == seed));
    assert!(hits.len() <= 5);
}

#[test]
fn rebuild_from_iter_repopulates_index() {
    // Worker-side recovery path: index file lost or corrupted, we
    // rebuild from the SQLite store. This pins that bulk-loading
    // (via a closure or iter) gives the same query behavior.
    let idx = AnnIndex::open_in_memory(DIM, 16).expect("open");
    let pairs: Vec<(TrackId, Vec<f32>)> = (0..DIM)
        .map(|i| (TrackId::from(format!("t{i}")), unit_at(i)))
        .collect();

    idx.rebuild_from(pairs.iter().map(|(t, v)| (t, v.as_slice())))
        .expect("rebuild");

    assert_eq!(idx.len().expect("len"), DIM);
    let hits = idx.query(&unit_at(2), 1).expect("query");
    assert_eq!(hits[0].track_id.as_str(), "t2");
}

#[test]
fn similarity_is_cosine_for_l2_normalized_inputs() {
    // CLAP outputs are L2-normalized, so cosine == dot product.
    // We pre-normalize a non-axis vector and assert the similarity
    // matches the analytical cosine.
    let idx = AnnIndex::open_in_memory(DIM, 16).expect("open");
    let v: Vec<f32> = (0..DIM)
        .map(|i| u16::try_from(i).expect("DIM fits in u16").into())
        .collect();
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    let v_norm: Vec<f32> = v.iter().map(|x| x / norm).collect();
    idx.upsert(&TrackId::from("v"), &v_norm).expect("upsert");

    let hits: Vec<AnnQueryResult> = idx.query(&v_norm, 1).expect("query");
    assert!(
        (hits[0].similarity - 1.0).abs() < 1e-5,
        "self-similarity should be ~1.0, got {}",
        hits[0].similarity
    );
}
