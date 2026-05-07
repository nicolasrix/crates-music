//! Subsonic token + salt authentication.
//!
//! Per the Subsonic API spec, the token is `md5(password + salt)`. The
//! salt is sent in the clear alongside the token; the password never is.
//! A fresh random salt should be generated per request.

use md5::{Digest, Md5};
use rand::Rng;
use rand::distributions::Alphanumeric;

pub fn compute_token(password: &str, salt: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(password.as_bytes());
    hasher.update(salt.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn random_salt() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(12)
        .map(char::from)
        .collect()
}
