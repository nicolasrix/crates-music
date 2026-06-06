//! `music sync` subcommand: small client over the gateway sync surface.
//!
//! Three actions:
//! - `state`: GET /v1/sync/snapshot, pretty-print as JSON.
//! - `push`:  one POST /v1/sync/ops per track id, idempotent on item_id.
//! - `watch`: WS /v1/sync, print every server frame as one JSON line.
//!
//! The CLI doesn't drive playback off sync state — that's a future
//! integration. For now, this is enough to inspect what other devices
//! are doing and to push tracks from the terminal.

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use music_core::{QueueItemId, TrackId};
use music_sync::{ServerMessage, SyncOp};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::config::Config;
use crate::gateway::{http_client, require_gateway, ws_url_for};

pub async fn run_state(config: &Config) -> Result<()> {
    let gw = require_gateway(config)?;
    let url = format!("{}/v1/sync/snapshot", gw.url.trim_end_matches('/'));
    let body: serde_json::Value = http_client(gw)?
        .get(&url)
        .bearer_auth(&gw.bearer_token)
        .send()
        .await
        .context("requesting snapshot")?
        .error_for_status()
        .context("snapshot returned error status")?
        .json()
        .await
        .context("parsing snapshot body")?;
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(())
}

pub async fn run_push(config: &Config, track_ids: &[String]) -> Result<()> {
    let gw = require_gateway(config)?;
    let url = format!("{}/v1/sync/ops", gw.url.trim_end_matches('/'));
    let client = http_client(gw)?;
    for track_id in track_ids {
        let item_id = new_item_id();
        let op = SyncOp::Push {
            item_id: QueueItemId::from(item_id.clone()),
            track_id: TrackId::from(track_id.clone()),
        };
        let resp = client
            .post(&url)
            .bearer_auth(&gw.bearer_token)
            .json(&op)
            .send()
            .await
            .with_context(|| format!("pushing track {track_id}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("push of {track_id} rejected ({status}): {body}");
        }
        let ack: serde_json::Value = resp.json().await.context("parsing op ack")?;
        println!(
            "pushed {track_id} as item {item_id} (version {})",
            ack["version"]
        );
    }
    Ok(())
}

pub async fn run_watch(config: &Config) -> Result<()> {
    let gw = require_gateway(config)?;
    let mut ws_url = ws_url_for(&gw.url, "/v1/sync")?;
    // Browser parity: pass the bearer token as `access_token=` rather
    // than via the Authorization header. The gateway accepts both;
    // query-string auth keeps the WS handshake plumbing trivial.
    ws_url
        .query_pairs_mut()
        .append_pair("access_token", &gw.bearer_token);

    let (mut ws, _resp) = connect_async(ws_url.as_str())
        .await
        .context("connecting WS")?;
    while let Some(frame) = ws.next().await {
        match frame.context("WS protocol error")? {
            Message::Text(t) => {
                // Validate the frame so a malformed payload surfaces,
                // but emit the raw text so the CLI's stdout remains a
                // faithful tee of the server's output.
                let _: ServerMessage = serde_json::from_str(&t).unwrap_or(ServerMessage::OpError {
                    message: "unknown frame".into(),
                });
                println!("{t}");
            }
            Message::Close(_) => break,
            _ => {} // ignore Ping/Pong/Binary
        }
    }
    Ok(())
}

/// Short, lexically-sortable item id. Server doesn't care about the
/// shape — clients pick one and Push is idempotent on collisions.
/// Uniqueness within a single CLI invocation is sufficient (there's no
/// concurrent CLI usage from one terminal); the millisecond prefix
/// covers cross-invocation uniqueness for the rare case of two
/// invocations within the same millisecond.
fn new_item_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0u64, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    format!("cli-{ms:x}-{n:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_ids_are_unique_within_a_burst() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            assert!(seen.insert(new_item_id()), "duplicate item id");
        }
    }
}
