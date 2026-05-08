//! Browser session token utilities + types.
//!
//! The store works with the *hash* of a token; the plaintext only ever
//! exists in three places: the OS RNG that minted it, the `Set-Cookie`
//! response, and the user's browser. By the time it reaches the
//! database column it's already SHA-256'd.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// A freshly minted session: contains both the plaintext token (to put
/// in the `Set-Cookie` header) and its hash (already stored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedSession {
    pub token: String,
    pub token_hash: String,
    pub issued_at_unix_ms: i64,
    pub expires_at_unix_ms: i64,
}

/// A session row looked up by token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub token_hash: String,
    pub issued_at_unix_ms: i64,
    pub expires_at_unix_ms: i64,
}

/// Mint a 32-byte random token, base64url-encoded (~43 chars). Suitable
/// for a session cookie value.
pub fn mint_token() -> String {
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// SHA-256 hex of the token. Used everywhere the DB sees it.
pub fn hash_token(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    let digest = h.finalize();
    let mut s = String::with_capacity(64);
    for b in &digest {
        use std::fmt::Write;
        write!(&mut s, "{b:02x}").expect("write to String never fails");
    }
    s
}
