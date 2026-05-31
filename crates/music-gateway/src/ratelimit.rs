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

#[cfg(test)]
mod tests {
    use super::*;

    fn ip() -> IpAddr {
        IpAddr::from([10, 0, 0, 7])
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
