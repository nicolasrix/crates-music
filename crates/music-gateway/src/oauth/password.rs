//! Argon2id password hashing.
//!
//! Pure functions over `&str` — the store deals in the resulting PHC
//! string, so storage doesn't depend on argon2 directly.

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
