//! Small in-memory rate limiters for the gateway's most-abusable
//! endpoints. Deliberately minimal — single-user, LAN-scoped, no
//! distributed coordination needed.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Consecutive failed `/oauth/login` attempts from one source before it
/// is locked out. The master password is the root credential; combined
/// with Argon2id's deliberate per-verify cost this makes online
/// brute-force impractical.
const MAX_FAILURES: u32 = 5;

/// How long a source stays locked out after tripping `MAX_FAILURES`.
/// Short on purpose: behind a reverse proxy every client collapses to
/// the proxy's IP (see `LoginLimiter`), so a long window would let one
/// attacker lock out the legitimate user. 60 s caps that DoS while
/// still throttling a guessing loop to ~5 tries/min.
const LOCKOUT: Duration = Duration::from_mins(1);

#[derive(Debug, Default)]
struct Attempt {
    failures: u32,
    locked_until: Option<Instant>,
}

/// Per-source failed-login limiter.
///
/// Keyed on the peer `IpAddr`. NOTE: when the gateway runs behind a
/// reverse proxy (the production Caddy deployment) the peer address is
/// the proxy's, so all clients share one bucket and the limiter behaves
/// as a global throttle. We intentionally do *not* trust
/// `X-Forwarded-For` for keying — an attacker could rotate it to bypass
/// the lockout entirely. The proxy-collapsed global behaviour is the
/// safe failure mode (throttle everyone briefly) rather than the unsafe
/// one (throttle no one).
#[derive(Debug, Default)]
pub struct LoginLimiter {
    attempts: Mutex<HashMap<IpAddr, Attempt>>,
}

impl LoginLimiter {
    /// Returns `Err(remaining)` if `ip` is currently locked out. A
    /// lockout whose window has elapsed is reset in passing, so the
    /// next failure starts a fresh count.
    pub fn check(&self, ip: IpAddr) -> Result<(), Duration> {
        let now = Instant::now();
        let mut map = self.attempts.lock().expect("login limiter poisoned");
        if let Some(a) = map.get_mut(&ip)
            && let Some(until) = a.locked_until
        {
            if until > now {
                return Err(until - now);
            }
            // Window elapsed — clear so the counter restarts.
            *a = Attempt::default();
        }
        Ok(())
    }

    /// Record a failed attempt; trips the lockout at `MAX_FAILURES`.
    pub fn record_failure(&self, ip: IpAddr) {
        let now = Instant::now();
        let mut map = self.attempts.lock().expect("login limiter poisoned");
        let a = map.entry(ip).or_default();
        a.failures += 1;
        if a.failures >= MAX_FAILURES {
            a.locked_until = Some(now + LOCKOUT);
        }
    }

    /// Clear all state for a source after a successful login.
    pub fn record_success(&self, ip: IpAddr) {
        let mut map = self.attempts.lock().expect("login limiter poisoned");
        map.remove(&ip);
    }
}

/// Cap on the number of distinct `(endpoint, source)` buckets the
/// [`RateLimiter`] tracks. The gateway is internet-reachable, so an
/// attacker rotating source IPs could otherwise grow the map without
/// bound — a memory-exhaustion DoS that defeats the very endpoints this
/// limiter protects. When the map exceeds this, expired windows are
/// swept before inserting a new key.
const MAX_TRACKED_SOURCES: usize = 10_000;

#[derive(Debug)]
struct RateWindow {
    count: u32,
    started: Instant,
}

/// Fixed-window request-rate limiter for the public, unauthenticated
/// OAuth endpoints (`/oauth/guest`, `/oauth/device_authorization`,
/// `/oauth/revoke`). Unlike [`LoginLimiter`] — which counts *failures* —
/// these endpoints have no pass/fail signal (RFC 7009 revoke always
/// answers 200; device-auth and guest redemption "succeed" structurally
/// on every call), so we cap the *request rate* per source instead.
///
/// Keyed on `(endpoint, IpAddr)` so hammering one endpoint can't throttle
/// a legitimate caller on another. Same reverse-proxy caveat as
/// `LoginLimiter`: behind Caddy every peer collapses to the proxy IP and
/// each endpoint's bucket becomes a global throttle — the safe failure
/// mode for endpoints that are rare human actions. `X-Forwarded-For` is
/// deliberately not trusted for keying (an attacker would rotate it).
#[derive(Debug, Default)]
pub struct RateLimiter {
    windows: Mutex<HashMap<(&'static str, IpAddr), RateWindow>>,
}

impl RateLimiter {
    /// Count one request against `(endpoint, ip)`. Returns `Ok(())` when
    /// the caller is within `max` requests for the current `window`, or
    /// `Err(retry_after)` when over. A window whose span has elapsed
    /// resets in passing, so the next request starts a fresh count.
    pub fn check(
        &self,
        endpoint: &'static str,
        ip: IpAddr,
        max: u32,
        window: Duration,
    ) -> Result<(), Duration> {
        let now = Instant::now();
        let mut map = self.windows.lock().expect("rate limiter poisoned");

        // Bound the map: if it has grown large, drop windows that have
        // fully expired (they'd reset on next use anyway) before adding
        // a new source. O(n) but only when the map is already big.
        if map.len() >= MAX_TRACKED_SOURCES && !map.contains_key(&(endpoint, ip)) {
            map.retain(|_, w| now.duration_since(w.started) < window);
        }

        let w = map.entry((endpoint, ip)).or_insert(RateWindow {
            count: 0,
            started: now,
        });
        let elapsed = now.duration_since(w.started);
        if elapsed >= window {
            w.count = 0;
            w.started = now;
        }
        if w.count >= max {
            // Time until the current window rolls over.
            return Err(window.saturating_sub(now.duration_since(w.started)));
        }
        w.count += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip() -> IpAddr {
        IpAddr::from([10, 0, 0, 7])
    }

    #[test]
    fn rate_limiter_allows_up_to_max_then_429s() {
        let lim = RateLimiter::default();
        let win = Duration::from_secs(60);
        // First `max` requests pass.
        for _ in 0..3 {
            assert!(lim.check("guest", ip(), 3, win).is_ok());
        }
        // The next is over the limit.
        assert!(lim.check("guest", ip(), 3, win).is_err());
    }

    #[test]
    fn rate_limiter_buckets_are_independent_per_endpoint() {
        let lim = RateLimiter::default();
        let win = Duration::from_secs(60);
        // Exhaust the guest bucket.
        for _ in 0..2 {
            assert!(lim.check("guest", ip(), 2, win).is_ok());
        }
        assert!(lim.check("guest", ip(), 2, win).is_err());
        // A different endpoint for the same IP is unaffected.
        assert!(lim.check("revoke", ip(), 2, win).is_ok());
    }

    #[test]
    fn rate_limiter_window_resets_after_elapse() {
        let lim = RateLimiter::default();
        // A zero-length window means every request sees an elapsed window
        // and resets, so the cap never trips.
        let zero = Duration::from_secs(0);
        for _ in 0..10 {
            assert!(lim.check("guest", ip(), 1, zero).is_ok());
        }
    }

    #[test]
    fn allows_until_threshold_then_locks() {
        let lim = LoginLimiter::default();
        // First MAX_FAILURES-1 failures do not lock.
        for _ in 0..MAX_FAILURES - 1 {
            assert!(lim.check(ip()).is_ok());
            lim.record_failure(ip());
        }
        // Still allowed to try once more...
        assert!(lim.check(ip()).is_ok());
        // ...and that try is the one that trips the lockout.
        lim.record_failure(ip());
        assert!(lim.check(ip()).is_err());
    }

    #[test]
    fn success_clears_failures() {
        let lim = LoginLimiter::default();
        for _ in 0..MAX_FAILURES - 1 {
            lim.record_failure(ip());
        }
        lim.record_success(ip());
        // Counter reset: a fresh failure does not immediately lock.
        lim.record_failure(ip());
        assert!(lim.check(ip()).is_ok());
    }

    #[test]
    fn distinct_ips_are_independent() {
        let lim = LoginLimiter::default();
        let other = IpAddr::from([10, 0, 0, 8]);
        for _ in 0..MAX_FAILURES {
            lim.record_failure(ip());
        }
        assert!(lim.check(ip()).is_err());
        // A different source is unaffected.
        assert!(lim.check(other).is_ok());
    }
}
