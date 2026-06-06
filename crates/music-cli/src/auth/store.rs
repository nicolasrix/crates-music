//! On-disk token store for the gateway Device Authorization Grant.
//!
//! Holds the OAuth tokens obtained via `music auth login`, in a
//! `cli-tokens.json` file next to the config. The file is the CLI's only
//! gateway credential (there is no static bearer anymore), so it's written
//! `0600` on Unix.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The persisted gateway credentials. `access_expires_at_ms` is an
/// absolute deadline (computed from the token response's `expires_in` at
/// save time) so a stale clock between invocations can't be fooled by a
/// relative TTL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTokens {
    pub client_id: String,
    pub access_token: String,
    pub refresh_token: String,
    pub access_expires_at_ms: i64,
}

impl StoredTokens {
    /// Build a store entry from a token-endpoint response, stamping the
    /// absolute access-token expiry from `expires_in` (seconds).
    pub fn from_response(client_id: &str, access_token: String, refresh_token: String, expires_in: u64) -> Self {
        let expires_at = now_ms().saturating_add(i64::try_from(expires_in.saturating_mul(1000)).unwrap_or(i64::MAX));
        Self {
            client_id: client_id.to_string(),
            access_token,
            refresh_token,
            access_expires_at_ms: expires_at,
        }
    }
}

/// Load the token store. `Ok(None)` when the file doesn't exist (not yet
/// authenticated); an unreadable or malformed file is an error.
pub fn load(path: &Path) -> Result<Option<StoredTokens>> {
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            let tokens = serde_json::from_str(&raw)
                .with_context(|| format!("parsing token store at {}", path.display()))?;
            Ok(Some(tokens))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading token store at {}", path.display())),
    }
}

/// Persist the token store, creating the parent dir if needed and writing
/// the file `0600` (Unix) so the tokens aren't world-readable.
pub fn save(path: &Path, tokens: &StoredTokens) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating token store dir {}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(tokens).context("serializing tokens")?;
    write_private(path, json.as_bytes())
        .with_context(|| format!("writing token store at {}", path.display()))
}

/// Delete the token store. Missing file is not an error (idempotent logout).
pub fn delete(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("deleting token store at {}", path.display())),
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

/// Current unix time in milliseconds.
pub fn now_ms() -> i64 {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    i64::try_from(ms).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("crates-cli-tok-{}", std::process::id()));
        let path = dir.join("cli-tokens.json");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(load(&path).unwrap().is_none(), "absent store loads as None");

        let tokens = StoredTokens {
            client_id: "cli".into(),
            access_token: "acc".into(),
            refresh_token: "ref".into(),
            access_expires_at_ms: 123,
        };
        save(&path, &tokens).unwrap();
        assert_eq!(load(&path).unwrap().unwrap(), tokens);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "token store must be private");
        }

        delete(&path).unwrap();
        assert!(load(&path).unwrap().is_none());
        delete(&path).unwrap(); // idempotent
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn from_response_sets_future_expiry() {
        let before = now_ms();
        let t = StoredTokens::from_response("cli", "a".into(), "r".into(), 3600);
        assert!(t.access_expires_at_ms >= before + 3600 * 1000);
        assert_eq!(t.client_id, "cli");
    }
}
