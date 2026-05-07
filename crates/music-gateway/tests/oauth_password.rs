//! Argon2id password hashing — pure-function level. The store wires these
//! into `users.password_hash`; that wiring is exercised separately.

use music_gateway::oauth::password::{hash, verify};

#[test]
fn hash_then_verify_roundtrips() {
    let phc = hash("correct horse battery staple").unwrap();
    assert!(verify("correct horse battery staple", &phc).unwrap());
}

#[test]
fn verify_rejects_wrong_password() {
    let phc = hash("right").unwrap();
    assert!(!verify("wrong", &phc).unwrap());
}

#[test]
fn hashes_use_unique_salts() {
    // The PHC string embeds the salt, so two hashes of the same input
    // must differ — otherwise we leak that two passwords match.
    let a = hash("same").unwrap();
    let b = hash("same").unwrap();
    assert_ne!(a, b, "every hash must use a fresh salt");
}

#[test]
fn verify_errors_on_malformed_phc() {
    // A bad PHC string is a *programmer error* (corrupted DB row), not an
    // auth failure — surface as Err, not Ok(false).
    let result = verify("anything", "not-a-real-phc-string");
    assert!(result.is_err(), "malformed PHC must be a hard error");
}
