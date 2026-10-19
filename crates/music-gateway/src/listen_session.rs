//! Listening sessions: one id per stretch of continuous listening, split on
//! idle gaps.
//!
//! The event log's `session_id` column is the *recommend*-session (an
//! autoplay anchor), which can stay open for days, so it can't tell you
//! which plays happened together. Every logged event therefore also gets a
//! `listen_session` in its metadata: a new one whenever the user has been
//! idle for longer than [`LISTEN_SESSION_GAP_MS`]. That is the sequence
//! boundary a behavioural model (track2vec and friends) trains on.
//!
//! Stamped server-side so every client agrees on one definition. State is
//! one small entry per user, seeded from the log after a restart so a
//! gateway reboot mid-listen doesn't split the session.

use std::collections::HashMap;

use music_recommend::EventStore;
use tokio::sync::Mutex;

/// Idle time that ends a listening session. 30 minutes is the usual
/// inactivity cutoff in listening-session research.
pub const LISTEN_SESSION_GAP_MS: i64 = 30 * 60_000;

/// A user's open listening session: its id and the end of the latest
/// activity seen in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Current {
    pub id: String,
    pub last_ms: i64,
}

/// The session that activity spanning `start_ms..=end_ms` belongs to.
///
/// Continues `prev` when the activity started within `gap_ms` of the last
/// seen, otherwise opens a new session named after its start. An event
/// that arrives late (starts before `last_ms`) joins the open session and
/// never moves `last_ms` backwards.
pub fn advance(prev: Option<&Current>, start_ms: i64, end_ms: i64, gap_ms: i64) -> Current {
    match prev {
        Some(cur) if start_ms - cur.last_ms <= gap_ms => Current {
            id: cur.id.clone(),
            last_ms: cur.last_ms.max(end_ms),
        },
        _ => Current {
            id: format!("ls-{start_ms}"),
            last_ms: end_ms,
        },
    }
}

/// Per-user open sessions.
#[derive(Debug, Default)]
pub struct ListenSessions {
    current: Mutex<HashMap<i64, Current>>,
}

impl ListenSessions {
    /// The listening session for `user_id`'s activity spanning
    /// `start_ms..=end_ms`, advancing the user's open session.
    pub async fn stamp(
        &self,
        store: &EventStore,
        user_id: i64,
        start_ms: i64,
        end_ms: i64,
    ) -> String {
        let mut current = self.current.lock().await;
        if !current.contains_key(&user_id) {
            // First event since boot: resume the session the log ended on.
            match store.last_listen_session(user_id).await {
                Ok(Some((id, last_ms))) => {
                    current.insert(user_id, Current { id, last_ms });
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "listen session: resume lookup failed; starting fresh"
                    );
                }
            }
        }
        let next = advance(current.get(&user_id), start_ms, end_ms, LISTEN_SESSION_GAP_MS);
        let id = next.id.clone();
        current.insert(user_id, next);
        id
    }
}

/// `metadata` with `fields` merged in, as a new value. `None` becomes an
/// object; a non-object (no client of ours sends one) is returned unchanged
/// rather than discarded.
pub fn with_fields(
    metadata: Option<&serde_json::Value>,
    fields: impl IntoIterator<Item = (&'static str, serde_json::Value)>,
) -> Option<serde_json::Value> {
    let mut obj = match metadata {
        None => serde_json::Map::new(),
        Some(serde_json::Value::Object(m)) => m.clone(),
        Some(other) => return Some(other.clone()),
    };
    for (k, v) in fields {
        obj.insert(k.to_string(), v);
    }
    Some(serde_json::Value::Object(obj))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MIN: i64 = 60_000;
    const GAP: i64 = 30 * MIN;

    #[test]
    fn first_activity_opens_a_session_named_after_its_start() {
        let s = advance(None, 5 * MIN, 9 * MIN, GAP);
        assert_eq!(s, Current { id: format!("ls-{}", 5 * MIN), last_ms: 9 * MIN });
    }

    #[test]
    fn activity_within_the_gap_continues_the_session() {
        let prev = advance(None, 0, 4 * MIN, GAP);
        let next = advance(Some(&prev), 30 * MIN, 34 * MIN, GAP);
        assert_eq!(next.id, prev.id);
        assert_eq!(next.last_ms, 34 * MIN);
    }

    #[test]
    fn activity_after_the_gap_opens_a_new_session() {
        let prev = advance(None, 0, 4 * MIN, GAP);
        let next = advance(Some(&prev), 35 * MIN, 36 * MIN, GAP);
        assert_ne!(next.id, prev.id);
    }

    #[test]
    fn late_event_joins_without_rewinding() {
        let prev = advance(None, 0, 20 * MIN, GAP);
        let next = advance(Some(&prev), 2 * MIN, 3 * MIN, GAP);
        assert_eq!(next, prev);
    }

    #[test]
    fn with_fields_merges_into_existing_metadata() {
        let m = json!({"played_ms": 5});
        let out = with_fields(Some(&m), [("listen_session", json!("ls-1"))]).unwrap();
        assert_eq!(out, json!({"played_ms": 5, "listen_session": "ls-1"}));
        assert_eq!(m, json!({"played_ms": 5}), "input untouched");
    }

    #[test]
    fn with_fields_creates_an_object_from_none() {
        let out = with_fields(None, [("listen_session", json!("ls-1"))]).unwrap();
        assert_eq!(out, json!({"listen_session": "ls-1"}));
    }

    #[test]
    fn with_fields_leaves_non_objects_alone() {
        let m = json!([1, 2]);
        assert_eq!(with_fields(Some(&m), [("k", json!(1))]), Some(json!([1, 2])));
    }
}
