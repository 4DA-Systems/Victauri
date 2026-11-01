//! Shared axum middleware for Victauri's localhost HTTP servers.
//!
//! Gated behind the `middleware` feature flag.  Provides thin middleware
//! wrappers around the pure-logic security primitives in [`super::security`].

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;

use crate::security::{self, RateLimiter, constant_time_eq, is_allowed_origin, is_localhost_host};

const BEARER_PREFIX_LEN: usize = "Bearer ".len();

/// Least time between log lines for one kind of refused request.
const REFUSAL_LOG_INTERVAL_MS: u64 = 10_000;

/// Rate limits the log line of one kind of refused request. The guards run ahead of the rate
/// limiter (so a refusal spends no token), which left their logging unbounded: any local process
/// or web page could flood the app's console, and under `victauri run` rotate real output out of
/// the capture. The lines also carry no request text, which a page could otherwise plant in logs
/// an agent reads.
struct RefusalLog {
    /// Milliseconds since the Unix epoch of the last line; 0 means none yet.
    last_ms: AtomicU64,
    /// Refusals since then.
    suppressed: AtomicU64,
}

impl RefusalLog {
    const fn new() -> Self {
        Self {
            last_ms: AtomicU64::new(0),
            suppressed: AtomicU64::new(0),
        }
    }

    /// Record a refusal at `now_ms`. `Some(n)` means log one line now, covering `n` earlier
    /// refusals that were not logged.
    fn admit(&self, now_ms: u64) -> Option<u64> {
        let last = self.last_ms.load(Ordering::Relaxed);
        let due = last == 0 || now_ms.saturating_sub(last) >= REFUSAL_LOG_INTERVAL_MS;
        if due
            && self
                .last_ms
                .compare_exchange(last, now_ms.max(1), Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            return Some(self.suppressed.swap(0, Ordering::Relaxed));
        }
        self.suppressed.fetch_add(1, Ordering::Relaxed);
        None
    }

    fn admit_now(&self) -> Option<u64> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        self.admit(now_ms)
    }
}

static AUTH_REFUSALS: RefusalLog = RefusalLog::new();
static HOST_REFUSALS: RefusalLog = RefusalLog::new();
static ORIGIN_REFUSALS: RefusalLog = RefusalLog::new();

/// Shared authentication state holding the optional Bearer token for the MCP
/// server.
#[derive(Clone)]
pub struct AuthState {
    /// The expected Bearer token, or `None` if authentication is disabled.
    pub token: Option<String>,
}

/// Axum middleware that validates the `Authorization: Bearer <token>` header
/// against [`AuthState`].
///
/// Case-insensitive prefix matching per RFC 7235.  Constant-time token
/// comparison via [`constant_time_eq`].
///
/// # Errors
///
/// Returns [`StatusCode::UNAUTHORIZED`] if the token is missing or invalid.
pub async fn require_auth(
    axum::extract::State(auth): axum::extract::State<Arc<AuthState>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(expected) = &auth.token else {
        return Ok(next.run(request).await);
    };

    if bearer_matches(request.headers(), expected) {
        Ok(next.run(request).await)
    } else {
        if let Some(suppressed) = AUTH_REFUSALS.admit_now() {
            tracing::warn!(
                "Victauri: rejected request — invalid or missing auth token \
                 ({suppressed} more since the last such line)"
            );
        }
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Whether `headers` carry `Authorization: Bearer <expected>` (scheme matched
/// case-insensitively per RFC 7235, token compared in constant time).
#[must_use]
pub fn bearer_matches(headers: &axum::http::HeaderMap, expected: &str) -> bool {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .filter(|v| {
            v.get(..BEARER_PREFIX_LEN)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("bearer "))
        })
        .is_some_and(|v| constant_time_eq(&v.as_bytes()[BEARER_PREFIX_LEN..], expected.as_bytes()))
}

/// Create a rate limiter with the default capacity of
/// [`DEFAULT_RATE_LIMIT`](security::DEFAULT_RATE_LIMIT) requests per second.
#[must_use]
pub fn default_rate_limiter() -> Arc<RateLimiter> {
    Arc::new(RateLimiter::new(security::DEFAULT_RATE_LIMIT))
}

/// Axum middleware that rejects requests with 429 when the token bucket is
/// exhausted.
///
/// # Errors
///
/// Returns [`StatusCode::TOO_MANY_REQUESTS`] with `Retry-After: 1` header when
/// the rate limit is exceeded.
pub async fn rate_limit(
    axum::extract::State(limiter): axum::extract::State<Arc<RateLimiter>>,
    request: Request,
    next: Next,
) -> Result<
    Response,
    (
        StatusCode,
        [(axum::http::HeaderName, axum::http::HeaderValue); 1],
    ),
> {
    if limiter.try_acquire() {
        Ok(next.run(request).await)
    } else {
        Err((
            StatusCode::TOO_MANY_REQUESTS,
            [(
                axum::http::header::RETRY_AFTER,
                axum::http::HeaderValue::from_static("1"),
            )],
        ))
    }
}

/// Axum middleware that blocks DNS rebinding attacks.
///
/// Rejects any request where the `Host` header is not a localhost address.
///
/// # Errors
///
/// Returns [`StatusCode::FORBIDDEN`] if the `Host` header is not `localhost`,
/// `127.0.0.1`, or `::1`.
pub async fn dns_rebinding_guard(request: Request, next: Next) -> Result<Response, StatusCode> {
    let host = request
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !is_localhost_host(host) {
        if let Some(suppressed) = HOST_REFUSALS.admit_now() {
            tracing::warn!(
                "Victauri: blocked a request whose Host header is not localhost (DNS rebinding \
                 guard; {suppressed} more since the last such line)"
            );
        }
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(next.run(request).await)
}

/// Axum middleware that blocks requests a web page made a browser send.
///
/// Runs before the rate limiter, so a refused request never spends a token (a page looping
/// `<img src="http://127.0.0.1:7373/health">` sends no `Origin` and used to drain the shared
/// bucket until the agent was answered 429).
///
/// # Errors
///
/// Returns [`StatusCode::FORBIDDEN`] if the request carries a `Sec-Fetch-Site` header other
/// than `none` (every modern browser marks page-initiated requests; `none` is a user
/// navigation), or an `Origin` header that is unreadable or not a localhost origin.
pub async fn origin_guard(request: Request, next: Next) -> Result<Response, StatusCode> {
    if let Some(site) = request.headers().get("sec-fetch-site")
        && site.as_bytes() != b"none"
    {
        if let Some(suppressed) = ORIGIN_REFUSALS.admit_now() {
            tracing::warn!(
                "Victauri: blocked a browser-initiated request ({suppressed} more since the last \
                 such line)"
            );
        }
        return Err(StatusCode::FORBIDDEN);
    }
    if let Some(origin) = request.headers().get("origin") {
        // An Origin that is not visible ASCII cannot be judged, so it is refused rather than
        // treated as absent (absent means "not a browser").
        match origin.to_str() {
            Ok(origin) if is_allowed_origin(origin) => {}
            _ => {
                if let Some(suppressed) = ORIGIN_REFUSALS.admit_now() {
                    tracing::warn!(
                        "Victauri: blocked a cross-origin request ({suppressed} more since the \
                         last such line)"
                    );
                }
                return Err(StatusCode::FORBIDDEN);
            }
        }
    }
    Ok(next.run(request).await)
}

/// Axum middleware that sets security-hardening response headers on every
/// response.
pub async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        axum::http::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    headers.insert(
        axum::http::header::HeaderName::from_static("x-frame-options"),
        axum::http::HeaderValue::from_static("DENY"),
    );
    // No `Access-Control-Allow-Origin` at all: `null` is not "deny" — it GRANTS the opaque
    // `null` origin (sandboxed iframes, `data:` / `file:` documents).
    headers.insert(
        axum::http::header::HeaderName::from_static("content-security-policy"),
        axum::http::HeaderValue::from_static("default-src 'none'"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_log_admits_one_line_per_interval_and_counts_the_rest() {
        let log = RefusalLog::new();
        assert_eq!(log.admit(1_000), Some(0), "the first refusal is logged");
        for t in 1_001..1_500 {
            assert_eq!(log.admit(t), None);
        }
        assert_eq!(
            log.admit(1_000 + REFUSAL_LOG_INTERVAL_MS),
            Some(499),
            "the next line says how many went unlogged"
        );
        assert_eq!(log.admit(1_001 + REFUSAL_LOG_INTERVAL_MS), None);
        // A clock that goes backwards never panics, and never logs early.
        assert_eq!(log.admit(5), None);
    }
}
