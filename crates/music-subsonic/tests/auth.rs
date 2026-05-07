//! Token + salt auth is the Subsonic standard: `t = md5(password + salt)`.
//! These are pure-function tests — no HTTP.

use music_subsonic::auth;

/// Canonical example from the Subsonic API spec
/// (<http://www.subsonic.org/pages/api.jsp>).
#[test]
fn compute_token_matches_subsonic_spec_example() {
    // password = "sesame", salt = "c19b2d"
    // → md5("sesamec19b2d") = "26719a1196d2a940705a59634eb18eab"
    let token = auth::compute_token("sesame", "c19b2d");
    assert_eq!(token, "26719a1196d2a940705a59634eb18eab");
}

#[test]
fn compute_token_is_lowercase_hex_32_chars() {
    let token = auth::compute_token("anything", "salt");
    assert_eq!(token.len(), 32);
    assert!(
        token
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
}

#[test]
fn compute_token_is_deterministic() {
    let a = auth::compute_token("p", "s");
    let b = auth::compute_token("p", "s");
    assert_eq!(a, b);
}

#[test]
fn compute_token_changes_with_salt() {
    let a = auth::compute_token("p", "salt-a");
    let b = auth::compute_token("p", "salt-b");
    assert_ne!(a, b);
}

#[test]
fn random_salt_is_usable_as_subsonic_salt() {
    // Subsonic recommends salt of length >= 6, alphanumeric.
    let salt = auth::random_salt();
    assert!(salt.len() >= 6);
    assert!(salt.chars().all(|c| c.is_ascii_alphanumeric()));
}
