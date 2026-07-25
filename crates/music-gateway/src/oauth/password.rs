//! Argon2id password hashing.
//!
//! Pure functions over `&str` — the store deals in the resulting PHC
//! string, so storage doesn't depend on argon2 directly.

use std::sync::OnceLock;

use argon2::password_hash::{
    PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng,
};
use argon2::{Argon2, password_hash};

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("argon2: {0}")]
    Argon2(password_hash::Error),
}

impl From<password_hash::Error> for PasswordError {
    fn from(e: password_hash::Error) -> Self {
        Self::Argon2(e)
    }
}

pub type Result<T> = std::result::Result<T, PasswordError>;

/// Hash a plaintext password with Argon2id and return its PHC string.
/// The PHC string embeds the salt and parameters so `verify` is fully
/// self-describing.
pub fn hash(plaintext: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let phc = Argon2::default()
        .hash_password(plaintext.as_bytes(), &salt)?
        .to_string();
    Ok(phc)
}

/// Verify `plaintext` against a previously stored PHC string. Returns
/// `Ok(false)` for a wrong password, `Err(_)` only for a malformed PHC —
/// callers shouldn't conflate "user typed wrong" with "DB row corrupted".
pub fn verify(plaintext: &str, phc: &str) -> Result<bool> {
    let parsed = PasswordHash::new(phc)?;
    match Argon2::default().verify_password(plaintext.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(password_hash::Error::Password) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Burn an Argon2id verify against a fixed dummy hash and return `false`.
///
/// Used on the login path when *no* master password is stored yet, so
/// that the "not bootstrapped" branch costs the same wall-clock as a
/// real wrong-password verify. Without this, an unauthenticated caller
/// could distinguish "gateway not configured" from "wrong password" by
/// response latency alone — the same bootstrap-state leak the uniform
/// 401 closes at the status-code level. The dummy hash uses
/// `Argon2::default()` (identical params to real hashes), so the timing
/// matches.
pub fn verify_absent(plaintext: &str) -> bool {
    static DUMMY_PHC: OnceLock<String> = OnceLock::new();
    let phc =
        DUMMY_PHC.get_or_init(|| hash("placeholder-never-matches").expect("hashing a constant"));
    // A dummy hash never matches; the call is purely for its timing.
    let _ = verify(plaintext, phc);
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify_roundtrips() {
        let phc = hash("correct horse battery staple").unwrap();
        assert!(verify("correct horse battery staple", &phc).unwrap());
        assert!(!verify("wrong", &phc).unwrap());
    }

    #[test]
    fn verify_absent_always_false() {
        // Whatever the input, the no-stored-hash path reports failure.
        assert!(!verify_absent(""));
        assert!(!verify_absent("anything-at-all"));
    }
}
