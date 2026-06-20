//! Shared security primitives for Victauri's localhost HTTP server.
//!
//! This module provides the pure-logic building blocks that `victauri-plugin`
//! uses in its axum middleware stack. Keeping them here (rather than inline in the
//! plugin) keeps the security logic unit-testable without a Tauri runtime.

use std::sync::atomic::{AtomicU64, Ordering};

// ── Constant-time comparison ─────────────────────────────────────────────

/// Constant-time byte comparison to prevent timing side-channel attacks on
/// token validation.
///
/// Returns `true` only when `a` and `b` are the same length **and** every
/// byte matches.  The comparison always examines every byte so that the
/// execution time depends only on the length, never on where the first
/// mismatch occurs.
#[must_use]
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

// ── Token generation ─────────────────────────────────────────────────────

/// Generate a random UUID v4 token suitable for Bearer authentication.
#[must_use]
pub fn generate_token() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ── Rate limiter ─────────────────────────────────────────────────────────

/// Lock-free token-bucket rate limiter using monotonic timestamps for smooth
/// refill.
///
/// Uses [`std::time::Instant`] instead of `SystemTime` so the refill clock is
/// immune to NTP adjustments and pre-epoch system clocks.
///
/// Thread-safe via `AtomicU64` — no mutexes, no allocations on the hot path.
pub struct RateLimiter {
    tokens: AtomicU64,
    max_tokens: u64,
    last_refill_ms: AtomicU64,
    refill_rate_per_sec: u64,
    epoch: std::time::Instant,
}

impl RateLimiter {
    /// Create a rate limiter with the given maximum requests per second.
    #[must_use]
    pub fn new(max_requests_per_sec: u64) -> Self {
        Self {
            tokens: AtomicU64::new(max_requests_per_sec),
            max_tokens: max_requests_per_sec,
            last_refill_ms: AtomicU64::new(0),
            refill_rate_per_sec: max_requests_per_sec,
            epoch: std::time::Instant::now(),
        }
    }

    /// Atomically consume one token, returning `true` if the request is
    /// allowed.
    pub fn try_acquire(&self) -> bool {
        self.refill();
        loop {
            let current = self.tokens.load(Ordering::Relaxed);
            if current == 0 {
                return false;
            }
            if self
                .tokens
                .compare_exchange_weak(current, current - 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Maximum token capacity.
    #[must_use]
    pub fn max_tokens(&self) -> u64 {
        self.max_tokens
    }

    /// Current token count (snapshot — may change immediately after reading).
    #[must_use]
    pub fn current_tokens(&self) -> u64 {
        self.tokens.load(Ordering::Relaxed)
    }

    fn elapsed_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn refill(&self) {
        let now = self.elapsed_ms();
        let last = self.last_refill_ms.load(Ordering::Relaxed);
        let elapsed_ms = now.saturating_sub(last);
        if elapsed_ms == 0 {
            return;
        }
        let add = elapsed_ms * self.refill_rate_per_sec / 1000;
        if add == 0 {
            return;
        }
        if self
            .last_refill_ms
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            loop {
                let current = self.tokens.load(Ordering::Relaxed);
                let new_val = (current + add).min(self.max_tokens);
                if self
                    .tokens
                    .compare_exchange_weak(current, new_val, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
                {
                    break;
                }
            }
        }
    }
}

/// Default rate limit: 1 000 requests per second.
pub const DEFAULT_RATE_LIMIT: u64 = 1000;

// ── Host validation (DNS rebinding guard) ────────────────────────────────

/// Returns `true` if `host` (from the HTTP `Host` header) resolves to a
/// localhost address.
///
/// Handles `localhost`, `127.0.0.1`, `::1`, and any of those with a port
/// suffix (e.g. `localhost:7373`, `[::1]:7373`).
#[must_use]
pub fn is_localhost_host(host: &str) -> bool {
    let host_name = if let Some(rest) = host.strip_prefix('[') {
        // Bracketed IPv6: [::1] or [::1]:7373. The bytes after `]` MUST be empty or a
        // `:port` suffix — anything else (e.g. `[::1].evil.com`, `[::1]@x`) is rejected so a
        // bracket-prefixed host can't smuggle a non-localhost authority past the guard.
        match rest.split_once(']') {
            Some((inner, "")) => inner,
            Some((inner, after)) if after.strip_prefix(':').is_some_and(valid_port) => inner,
            _ => return false,
        }
    } else if host.contains("::") {
        // Bare IPv6 (no brackets): ::1
        host
    } else {
        // IPv4 or hostname, strip a valid port: 127.0.0.1:7373 → 127.0.0.1.
        match host.split_once(':') {
            Some((name, port)) if valid_port(port) => name,
            Some(_) => return false,
            None => host,
        }
    };
    host_name.eq_ignore_ascii_case("localhost") || matches!(host_name, "127.0.0.1" | "::1")
}

fn valid_port(port: &str) -> bool {
    !port.is_empty() && port.parse::<u16>().is_ok()
}

// ── Origin validation (cross-origin guard) ───────────────────────────────

/// Returns `true` if `origin` (from the HTTP `Origin` header) is a
/// localhost origin, a `tauri://` origin, or absent.
///
/// Uses [`url::Url::parse`] internally so that subdomain-smuggling attacks
/// like `localhost.evil.com` are caught by comparing the **parsed host**
/// rather than doing prefix matching.
#[must_use]
pub fn is_allowed_origin(origin: &str) -> bool {
    if origin.starts_with("tauri://") {
        return true;
    }
    let Ok(parsed) = url::Url::parse(origin) else {
        return false;
    };
    parsed.username().is_empty()
        && parsed.password().is_none()
        && matches!(parsed.scheme(), "http" | "https")
        && matches!(
            parsed.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        )
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // constant_time_eq

    #[test]
    fn ct_eq_equal() {
        assert!(constant_time_eq(b"secret-token-123", b"secret-token-123"));
    }

    #[test]
    fn ct_eq_different() {
        assert!(!constant_time_eq(b"secret-token-123", b"wrong-token-9999"));
    }

    #[test]
    fn ct_eq_different_lengths() {
        assert!(!constant_time_eq(b"short", b"longer-string"));
    }

    #[test]
    fn ct_eq_empty() {
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn ct_eq_one_empty() {
        assert!(!constant_time_eq(b"", b"notempty"));
        assert!(!constant_time_eq(b"notempty", b""));
    }

    #[test]
    fn ct_eq_single_bit_difference() {
        assert!(!constant_time_eq(b"A", b"B"));
    }

    #[test]
    fn ct_eq_long_strings() {
        let a = "a".repeat(10_000);
        let b = "a".repeat(10_000);
        assert!(constant_time_eq(a.as_bytes(), b.as_bytes()));
    }

    #[test]
    fn ct_eq_all_byte_values() {
        for b in 0..=255u8 {
            let a = [b];
            assert!(constant_time_eq(&a, &a));
            if b < 255 {
                assert!(!constant_time_eq(&a, &[b + 1]));
            }
        }
    }

    // generate_token

    #[test]
    fn tokens_are_unique() {
        let t1 = generate_token();
        let t2 = generate_token();
        assert_ne!(t1, t2);
        assert_eq!(t1.len(), 36);
    }

    #[test]
    fn token_is_valid_uuid() {
        let token = generate_token();
        assert!(uuid::Uuid::parse_str(&token).is_ok());
    }

    #[test]
    fn token_uniqueness_over_1000() {
        let mut set = std::collections::HashSet::new();
        for _ in 0..1000 {
            assert!(set.insert(generate_token()), "duplicate token");
        }
    }

    // RateLimiter

    #[test]
    fn rate_limiter_allows_within_budget() {
        let limiter = RateLimiter::new(10);
        for _ in 0..10 {
            assert!(limiter.try_acquire());
        }
    }

    #[test]
    fn rate_limiter_denies_when_exhausted() {
        let limiter = RateLimiter::new(5);
        for _ in 0..5 {
            assert!(limiter.try_acquire());
        }
        assert!(!limiter.try_acquire());
    }

    #[test]
    fn rate_limiter_initial_tokens_match_max() {
        let limiter = RateLimiter::new(42);
        assert_eq!(limiter.current_tokens(), 42);
        assert_eq!(limiter.max_tokens(), 42);
    }

    #[test]
    fn rate_limiter_zero_capacity() {
        let limiter = RateLimiter::new(0);
        assert!(!limiter.try_acquire());
    }

    #[test]
    fn rate_limiter_concurrent() {
        let limiter = std::sync::Arc::new(RateLimiter::new(1000));
        let mut handles = vec![];
        for _ in 0..10 {
            let l = limiter.clone();
            handles.push(std::thread::spawn(move || {
                let mut acquired: u64 = 0;
                for _ in 0..200 {
                    if l.try_acquire() {
                        acquired += 1;
                    }
                }
                acquired
            }));
        }
        let total: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert!(
            total >= 1000,
            "should dispense at least the initial budget, got {total}"
        );
        assert!(total <= 1200, "refill overshoot too high, got {total}");
    }

    // is_localhost_host

    #[test]
    fn host_allows_localhost() {
        assert!(is_localhost_host("localhost"));
        assert!(is_localhost_host("LOCALHOST"));
        assert!(is_localhost_host("localhost:7373"));
        assert!(is_localhost_host("LocalHost:7373"));
    }

    #[test]
    fn host_allows_ipv4() {
        assert!(is_localhost_host("127.0.0.1"));
        assert!(is_localhost_host("127.0.0.1:7373"));
    }

    #[test]
    fn host_allows_ipv6() {
        assert!(is_localhost_host("[::1]"));
        assert!(is_localhost_host("[::1]:7373"));
        assert!(is_localhost_host("::1"));
    }

    #[test]
    fn host_blocks_evil() {
        assert!(!is_localhost_host("evil.com"));
        assert!(!is_localhost_host("localhost.evil.com"));
        assert!(!is_localhost_host("127.0.0.1.evil.com"));
        assert!(!is_localhost_host(""));
    }

    #[test]
    fn host_blocks_bracketed_ipv6_smuggling() {
        // Bytes after `]` must be empty or a :port — a bracket-prefixed host cannot smuggle a
        // non-localhost authority past the guard (regression for the audit-prep A-F1 finding).
        assert!(!is_localhost_host("[::1].evil.com"));
        assert!(!is_localhost_host("[::1]@evil.com"));
        assert!(!is_localhost_host("[::1]evil"));
        assert!(!is_localhost_host("[2001:db8::1]")); // bracketed but not loopback
        assert!(!is_localhost_host("[::1].evil.com:7373")); // trailing-garbage-then-port
        // Valid bracketed loopback forms still pass (incl. a bracketed IPv4 loopback, which
        // still resolves to 127.0.0.1 — harmless, it's localhost either way).
        assert!(is_localhost_host("[::1]"));
        assert!(is_localhost_host("[::1]:7373"));
        assert!(is_localhost_host("[127.0.0.1]"));
    }

    #[test]
    fn host_blocks_malformed_port_suffixes() {
        assert!(!is_localhost_host("localhost:notaport"));
        assert!(!is_localhost_host("localhost:"));
        assert!(!is_localhost_host("localhost:7373:extra"));
        assert!(!is_localhost_host("127.0.0.1:notaport"));
        assert!(!is_localhost_host("[::1]:notaport"));
        assert!(!is_localhost_host("[::1]:"));
        assert!(!is_localhost_host("[::1]:7373:extra"));
        assert!(!is_localhost_host("[::1] :7373"));
    }

    // is_allowed_origin

    #[test]
    fn origin_allows_localhost_variants() {
        assert!(is_allowed_origin("http://localhost"));
        assert!(is_allowed_origin("http://localhost:7373"));
        assert!(is_allowed_origin("https://localhost"));
        assert!(is_allowed_origin("http://127.0.0.1"));
        assert!(is_allowed_origin("http://127.0.0.1:8080"));
        assert!(is_allowed_origin("http://[::1]"));
        assert!(is_allowed_origin("http://[::1]:7373"));
        assert!(is_allowed_origin("tauri://localhost"));
        assert!(is_allowed_origin("tauri://some-app"));
    }

    #[test]
    fn origin_blocks_smuggling() {
        assert!(!is_allowed_origin("http://localhost.evil.com"));
        assert!(!is_allowed_origin("https://127.0.0.1.evil.com"));
        assert!(!is_allowed_origin("http://localhost@evil.com"));
        assert!(!is_allowed_origin("http://user:pass@localhost:7373"));
    }

    #[test]
    fn origin_blocks_external() {
        assert!(!is_allowed_origin("http://evil.com"));
        assert!(!is_allowed_origin("https://attacker.io"));
        assert!(!is_allowed_origin("not-a-url"));
        assert!(!is_allowed_origin(""));
        assert!(!is_allowed_origin("null"));
        assert!(!is_allowed_origin("ftp://localhost"));
    }
}
