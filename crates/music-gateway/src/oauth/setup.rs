//! One-time setup token used to bootstrap the master password.
//!
//! On first start, the gateway has no `users` row. It generates a
//! 32-byte random token, prints a setup URL containing it, and accepts
//! exactly one POST to `/oauth/setup` with that token. After the call
//! succeeds, the token is wiped from memory and the endpoint refuses
//! everything for the rest of the process lifetime.

use std::sync::Arc;
use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use subtle::ConstantTimeEq;

#[derive(Debug, Clone)]
pub struct SetupToken {
    inner: Arc<Mutex<Option<String>>>,
}

impl SetupToken {
    /// Build a token that is *not* active — used when the gateway is
    /// already configured (master password already set).
    pub fn none() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
        }
    }

    /// Generate a fresh 32-byte token, base64url-encoded (~43 chars).
    pub fn generate() -> Self {
        let mut buf = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut buf);
        let s = URL_SAFE_NO_PAD.encode(buf);
        Self {
            inner: Arc::new(Mutex::new(Some(s))),
        }
    }

    /// Whether a token is currently held. Once `consume` runs, becomes
    /// `false` for the rest of the process lifetime.
    pub fn is_active(&self) -> bool {
        self.inner
            .lock()
            .expect("setup-token mutex never poisoned")
            .is_some()
    }

    /// Read the token's value (without consuming it). Used at startup
    /// to print the setup URL.
    pub fn value(&self) -> Option<String> {
        self.inner
            .lock()
            .expect("setup-token mutex never poisoned")
            .clone()
    }

    /// Constant-time comparison against the stored token. Returns
    /// `false` when the token is inactive (no comparison performed).
    pub fn matches(&self, presented: &str) -> bool {
        let guard = self.inner.lock().expect("setup-token mutex never poisoned");
        let Some(stored) = guard.as_deref() else {
            return false;
        };
        if stored.len() != presented.len() {
            // ct_eq panics on length mismatch in some impls; the length
            // alone isn't a secret here (token is fixed length).
            return false;
        }
        stored.as_bytes().ct_eq(presented.as_bytes()).into()
    }

    /// Take the token and clear it. Subsequent calls return `None`.
    pub fn consume(&self) -> Option<String> {
        self.inner
            .lock()
            .expect("setup-token mutex never poisoned")
            .take()
    }
}
