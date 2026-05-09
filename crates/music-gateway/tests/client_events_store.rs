//! Unit-level tests for the `client_events` table operations on
//! `TraceStore`. Server-side timestamps and user-agent are stamped at
//! the HTTP boundary, not inside the store — these tests just verify
//! round-trip persistence of fully-formed records.

use music_gateway::diagnostics::{ClientEventRecord, TraceStore};

fn rec(name: &str, occurred_ms: i64, value_ms: Option<f64>, page_path: &str) -> ClientEventRecord {
    ClientEventRecord {
        received_ms: 1_700_000_000_000,
        occurred_ms,
        session_id: "sess-abc".to_string(),
        name: name.to_string(),
        value_ms,
        rating: None,
        page_path: page_path.to_string(),
        user_agent: Some("Mozilla/5.0 test".to_string()),
        fields_json: "{}".to_string(),
    }
}

#[tokio::test]
async fn empty_store_returns_empty_client_events_list() {
    let store = TraceStore::open_in_memory().await.unwrap();
    let got = store.recent_client_events(50, None).await.unwrap();
    assert!(got.is_empty());
}

#[tokio::test]
async fn insert_batch_round_trips_newest_first() {
    let store = TraceStore::open_in_memory().await.unwrap();
    store
        .insert_client_events(vec![
            rec("web-vital.LCP", 1_700_000_000_500, Some(1234.5), "/albums"),
            rec("playback.start", 1_700_000_001_000, Some(187.0), "/albums/abc"),
        ])
        .await
        .unwrap();

    let got = store.recent_client_events(50, None).await.unwrap();
    assert_eq!(got.len(), 2);
    // Newest received-id first: second insert lands at index 0.
    assert_eq!(got[0].name, "playback.start");
    assert_eq!(got[1].name, "web-vital.LCP");
    assert_eq!(got[0].value_ms, Some(187.0));
}

#[tokio::test]
async fn name_filter_excludes_other_events() {
    let store = TraceStore::open_in_memory().await.unwrap();
    store
        .insert_client_events(vec![
            rec("web-vital.LCP", 1, Some(1.0), "/"),
            rec("web-vital.INP", 2, Some(50.0), "/"),
            rec("web-vital.LCP", 3, Some(2.0), "/"),
        ])
        .await
        .unwrap();

    let lcp = store
        .recent_client_events(50, Some("web-vital.LCP"))
        .await
        .unwrap();
    assert_eq!(lcp.len(), 2);
    assert!(lcp.iter().all(|e| e.name == "web-vital.LCP"));
}

#[tokio::test]
async fn empty_batch_is_a_noop() {
    let store = TraceStore::open_in_memory().await.unwrap();
    store.insert_client_events(vec![]).await.unwrap();
    assert!(store.recent_client_events(50, None).await.unwrap().is_empty());
}

#[tokio::test]
async fn null_value_ms_round_trips_for_non_timing_marks() {
    // Custom marks like "playback.user_skipped" don't carry a value_ms.
    let store = TraceStore::open_in_memory().await.unwrap();
    let mut r = rec("playback.user_skipped", 1, None, "/album/1");
    r.value_ms = None;
    store.insert_client_events(vec![r]).await.unwrap();
    let got = store.recent_client_events(10, None).await.unwrap();
    assert_eq!(got.len(), 1);
    assert!(got[0].value_ms.is_none());
}
