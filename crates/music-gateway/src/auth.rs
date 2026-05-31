//! Bearer-token middleware for protected routes.
//!
//! Accepts the bearer in either the `Authorization` header (RFC 6750
//! §2.1) or the `access_token` query parameter (RFC 6750 §2.3). The
//! query-param path exists because browser `<audio>` and `<img>`
//! elements can't add request headers — and stream URLs / cover-art
//! URLs go straight into those elements.
//!
//! After lifting the token from either source, we look it up in the
//! OAuth access_tokens table; on miss we fall back to the legacy static
//! bearer from config (constant-time comparison).

use axum::{
    extract::{Request, State},
    http::{StatusCode, header::AUTHORIZATION},
    middleware::Next,
    response::Response,
};

use crate::state::AppState;

pub async fn require_bearer(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let presented = extract_bearer(&request).ok_or(StatusCode::UNAUTHORIZED)?;

    // 1. OAuth access token? sha256 + index lookup; cheap.
    let oauth_ok = match state.oauth().find_access_token(&presented).await {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(e) => {
            tracing::error!("oauth access-token lookup failed: {e}");
            false
        }
    };
    if oauth_ok {
        return Ok(next.run(request).await);
    }

    // 2. Static bearer fallback (legacy CLI; goes away when CLI moves
    //    to OAuth in P4).
    if constant_time_eq(presented.as_bytes(), state.bearer_token().as_bytes()) {
        return Ok(next.run(request).await);
    }

    Err(StatusCode::UNAUTHORIZED)
}

/// Returns the bearer token string from either source, owned. We return
/// `String` (not `&str`) so the caller doesn't have to juggle borrows
/// across the request body.
fn extract_bearer(request: &Request) -> Option<String> {
    if let Some(header) = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
    {
        return Some(header.to_string());
    }
    let query = request.uri().query()?;
    for kv in query.split('&') {
        // Skip bare flags (`?autoplay`) rather than `?`-bailing the whole
        // function — a valueless param before `access_token` must not mask
        // a valid token.
        let Some((k, v)) = kv.split_once('=') else {
            continue;
        };
        if k == "access_token" {
            // The token is base64url ([A-Za-z0-9_-]) so percent-decoding
            // is identity, but be permissive in case a client encoded
            // anyway.
            return Some(percent_decode(v));
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut bytes = s.bytes().peekable();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let h1 = bytes.next();
            let h2 = bytes.next();
            if let (Some(h1), Some(h2)) = (h1, h2)
                && let (Some(d1), Some(d2)) = (hex_digit(h1), hex_digit(h2))
            {
                out.push(char::from(d1 * 16 + d2));
                continue;
            }
            // Malformed — preserve and move on.
            out.push('%');
            if let Some(h1) = h1 {
                out.push(char::from(h1));
            }
            if let Some(h2) = h2 {
                out.push(char::from(h2));
            }
        } else {
            out.push(char::from(b));
        }
    }
    out
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    fn req(uri: &str) -> Request {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    #[test]
    fn extracts_token_from_bearer_header() {
        let mut r = req("/v1/anything");
        r.headers_mut()
            .insert(AUTHORIZATION, "Bearer abc123".parse().unwrap());
        assert_eq!(extract_bearer(&r).as_deref(), Some("abc123"));
    }

    #[test]
    fn extracts_token_from_query_param() {
        assert_eq!(
            extract_bearer(&req("/v1/stream?access_token=abc123")).as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn bare_flag_before_token_does_not_mask_it() {
        // Regression: a valueless query param used to `?`-bail the whole
        // function, dropping a valid access_token after it.
        assert_eq!(
            extract_bearer(&req("/v1/stream?autoplay&access_token=abc123")).as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn no_token_returns_none() {
        assert_eq!(extract_bearer(&req("/v1/stream?autoplay&foo")), None);
    }
}
