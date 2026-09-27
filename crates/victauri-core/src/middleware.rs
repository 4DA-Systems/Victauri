//! Shared axum middleware for Victauri's localhost HTTP servers.
//!
//! Gated behind the `middleware` feature flag.  Provides thin middleware
//! wrappers around the pure-logic security primitives in [`super::security`].

use std::sync::Arc;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;

use crate::security::{self, RateLimiter, constant_time_eq, is_allowed_origin, is_localhost_host};

const BEARER_PREFIX_LEN: usize = "Bearer ".len();

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
        tracing::warn!("Victauri: rejected request — invalid or missing auth token");
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
        tracing::warn!("DNS rebinding attempt blocked: Host={host}");
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
        tracing::warn!("Browser-initiated request blocked: Sec-Fetch-Site={site:?}");
        return Err(StatusCode::FORBIDDEN);
    }
    if let Some(origin) = request.headers().get("origin") {
        // An Origin that is not visible ASCII cannot be judged, so it is refused rather than
        // treated as absent (absent means "not a browser").
        match origin.to_str() {
            Ok(origin) if is_allowed_origin(origin) => {}
            _ => {
                tracing::warn!("Cross-origin request blocked: Origin={origin:?}");
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
