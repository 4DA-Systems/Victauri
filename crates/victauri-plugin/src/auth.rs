use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use url::Url;

const BEARER_PREFIX_LEN: usize = "Bearer ".len();

/// Generate a random `UUID` v4 token suitable for Bearer authentication.
#[must_use]
pub fn generate_token() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Shared authentication state holding the optional Bearer token for the MCP server.
#[derive(Clone)]
pub struct AuthState {
    /// The expected Bearer token, or `None` if authentication is disabled.
    pub token: Option<String>,
}

/// Axum middleware that validates the `Authorization: Bearer <token>` header against [`AuthState`].
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

    let provided = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            let lower = v.to_lowercase();
            if lower.starts_with("bearer ") {
                Some(v[BEARER_PREFIX_LEN..].to_string())
            } else {
                None
            }
        });

    match provided {
        Some(ref token) if token == expected => Ok(next.run(request).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

// ── Rate Limiter ───────────────────────────────────────────────────────────

/// Lock-free token-bucket rate limiter using millisecond-precision timestamps for smooth refill.
pub struct RateLimiterState {
    tokens: AtomicU64,
    max_tokens: u64,
    last_refill_ms: AtomicU64,
    refill_rate_per_sec: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl RateLimiterState {
    /// Create a rate limiter with the given maximum requests per second.
    #[must_use]
    pub fn new(max_requests_per_sec: u64) -> Self {
        Self {
            tokens: AtomicU64::new(max_requests_per_sec),
            max_tokens: max_requests_per_sec,
            last_refill_ms: AtomicU64::new(now_ms()),
            refill_rate_per_sec: max_requests_per_sec,
        }
    }

    /// Atomically consume one token, returning `true` if the request is allowed.
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

    fn refill(&self) {
        let now = now_ms();
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

/// Axum middleware that rejects requests with 429 when the token bucket is exhausted.
///
/// # Errors
///
/// Returns [`StatusCode::TOO_MANY_REQUESTS`] if the token bucket has no remaining capacity.
pub async fn rate_limit(
    axum::extract::State(limiter): axum::extract::State<Arc<RateLimiterState>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if limiter.try_acquire() {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::TOO_MANY_REQUESTS)
    }
}

const DEFAULT_RATE_LIMIT: u64 = 1000;

/// Create a rate limiter with the default capacity of 1000 requests per second.
#[must_use]
pub fn default_rate_limiter() -> Arc<RateLimiterState> {
    Arc::new(RateLimiterState::new(DEFAULT_RATE_LIMIT))
}

// ── Security Middlewares ──────────────────────────────────────────────────

/// Axum middleware that blocks DNS rebinding attacks.
///
/// Rejects any request where the Host header is not a localhost address.
///
/// # Errors
///
/// Returns [`StatusCode::FORBIDDEN`] if the `Host` header is not `localhost`, `127.0.0.1`, or `::1`.
pub async fn dns_rebinding_guard(request: Request, next: Next) -> Result<Response, StatusCode> {
    let host = request
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let host_name = if host.starts_with('[') {
        host.split(']').next().map_or(host, |s| &s[1..])
    } else {
        host.split(':').next().unwrap_or(host)
    };
    let is_allowed = matches!(host_name, "localhost" | "127.0.0.1" | "::1" | "");
    if !is_allowed {
        tracing::warn!("DNS rebinding attempt blocked: Host={host}");
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(next.run(request).await)
}

/// Axum middleware that blocks cross-origin requests from browsers.
///
/// # Errors
///
/// Returns [`StatusCode::FORBIDDEN`] if the `Origin` header is present and does not match a
/// localhost or `tauri://` origin.
pub async fn origin_guard(request: Request, next: Next) -> Result<Response, StatusCode> {
    if let Some(origin) = request
        .headers()
        .get("origin")
        .and_then(|v| v.to_str().ok())
        && !is_allowed_origin(origin)
    {
        tracing::warn!("Cross-origin request blocked: Origin={origin}");
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(next.run(request).await)
}

fn is_allowed_origin(origin: &str) -> bool {
    if origin.starts_with("tauri://") {
        return true;
    }
    let Ok(parsed) = Url::parse(origin) else {
        return false;
    };
    matches!(parsed.scheme(), "http" | "https")
        && matches!(
            parsed.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        )
}

/// Axum middleware that sets security-hardening response headers on every response.
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
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::middleware;
    use axum::routing::get;
    use tower::ServiceExt; // for oneshot

    async fn ok_handler() -> &'static str {
        "ok"
    }

    #[test]
    fn token_generation_is_unique() {
        let t1 = generate_token();
        let t2 = generate_token();
        assert_ne!(t1, t2);
        assert_eq!(t1.len(), 36); // UUID v4 format
    }

    #[test]
    fn token_is_valid_uuid() {
        let token = generate_token();
        assert!(uuid::Uuid::parse_str(&token).is_ok());
    }

    #[test]
    fn rate_limiter_allows_within_budget() {
        let limiter = RateLimiterState::new(10);
        for _ in 0..10 {
            assert!(limiter.try_acquire());
        }
    }

    #[test]
    fn rate_limiter_denies_when_exhausted() {
        let limiter = RateLimiterState::new(5);
        for _ in 0..5 {
            assert!(limiter.try_acquire());
        }
        assert!(!limiter.try_acquire());
    }

    #[test]
    fn rate_limiter_initial_tokens_match_max() {
        let limiter = RateLimiterState::new(42);
        assert_eq!(limiter.tokens.load(Ordering::Relaxed), 42);
        assert_eq!(limiter.max_tokens, 42);
    }

    #[test]
    fn rate_limiter_concurrent_acquire() {
        // Use a large bucket so time-based refills (1 per second) are negligible
        let limiter = Arc::new(RateLimiterState::new(1000));
        let mut handles = vec![];
        for _ in 0..10 {
            let l = limiter.clone();
            handles.push(std::thread::spawn(move || {
                let mut acquired = 0;
                for _ in 0..200 {
                    if l.try_acquire() {
                        acquired += 1;
                    }
                }
                acquired
            }));
        }
        let total: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
        // All 1000 tokens should be dispensed; a time-based refill may add a few
        assert!((1000..=1010).contains(&total));
    }

    #[test]
    fn default_rate_limiter_has_expected_tokens() {
        let limiter = default_rate_limiter();
        assert_eq!(limiter.max_tokens, 1000);
    }

    #[test]
    fn rate_limiter_zero_capacity() {
        let limiter = RateLimiterState::new(0);
        assert!(!limiter.try_acquire());
    }

    // ── DNS Rebinding Guard tests ─────────────────────────────────────────

    fn dns_rebinding_router() -> Router {
        Router::new()
            .route("/test", get(ok_handler))
            .layer(middleware::from_fn(dns_rebinding_guard))
    }

    fn dns_request(host: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().uri("/test");
        if let Some(h) = host {
            builder = builder.header("host", h);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn dns_rebinding_allows_localhost() {
        let app = dns_rebinding_router();
        let resp = app.oneshot(dns_request(Some("localhost"))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn dns_rebinding_allows_127_0_0_1() {
        let app = dns_rebinding_router();
        let resp = app.oneshot(dns_request(Some("127.0.0.1"))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn dns_rebinding_allows_ipv6_bracketed() {
        let app = dns_rebinding_router();
        let resp = app.oneshot(dns_request(Some("[::1]"))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn dns_rebinding_allows_ipv6_bracketed_with_port() {
        let app = dns_rebinding_router();
        let resp = app.oneshot(dns_request(Some("[::1]:7373"))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn dns_rebinding_allows_ipv6_bare() {
        let app = dns_rebinding_router();
        let resp = app.oneshot(dns_request(Some("::1"))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn dns_rebinding_allows_empty_host() {
        let app = dns_rebinding_router();
        let resp = app.oneshot(dns_request(None)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn dns_rebinding_blocks_evil_com() {
        let app = dns_rebinding_router();
        let resp = app.oneshot(dns_request(Some("evil.com"))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn dns_rebinding_blocks_localhost_subdomain() {
        let app = dns_rebinding_router();
        let resp = app
            .oneshot(dns_request(Some("localhost.evil.com")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn dns_rebinding_blocks_ip_subdomain() {
        let app = dns_rebinding_router();
        let resp = app
            .oneshot(dns_request(Some("127.0.0.1.evil.com")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    // ── Origin Guard tests ────────────────────────────────────────────────

    fn origin_router() -> Router {
        Router::new()
            .route("/test", get(ok_handler))
            .layer(middleware::from_fn(origin_guard))
    }

    fn origin_request(origin: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().uri("/test");
        if let Some(o) = origin {
            builder = builder.header("origin", o);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn origin_allows_no_origin() {
        let app = origin_router();
        let resp = app.oneshot(origin_request(None)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn origin_allows_localhost_http() {
        let app = origin_router();
        let resp = app
            .oneshot(origin_request(Some("http://localhost:3000")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn origin_allows_127_0_0_1_https() {
        let app = origin_router();
        let resp = app
            .oneshot(origin_request(Some("https://127.0.0.1:8080")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn origin_allows_tauri_scheme() {
        let app = origin_router();
        let resp = app
            .oneshot(origin_request(Some("tauri://localhost")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn origin_blocks_null() {
        let app = origin_router();
        let resp = app.oneshot(origin_request(Some("null"))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn origin_blocks_evil_com() {
        let app = origin_router();
        let resp = app
            .oneshot(origin_request(Some("http://evil.com")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    // ── Security Headers tests ────────────────────────────────────────────

    fn security_headers_router() -> Router {
        Router::new()
            .route("/test", get(ok_handler))
            .layer(middleware::from_fn(security_headers))
    }

    #[tokio::test]
    async fn security_headers_x_content_type_options() {
        let app = security_headers_router();
        let req = Request::builder().uri("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
    }

    #[tokio::test]
    async fn security_headers_cache_control() {
        let app = security_headers_router();
        let req = Request::builder().uri("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("cache-control").unwrap(), "no-store");
    }

    #[tokio::test]
    async fn security_headers_x_frame_options() {
        let app = security_headers_router();
        let req = Request::builder().uri("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("x-frame-options").unwrap(), "DENY");
    }

    // ── Auth middleware integration tests ─────────────────────────────────

    fn auth_router(token: Option<&str>) -> Router {
        let state = Arc::new(AuthState {
            token: token.map(String::from),
        });
        Router::new()
            .route("/test", get(ok_handler))
            .layer(middleware::from_fn_with_state(state, require_auth))
    }

    fn auth_request(token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().uri("/test");
        if let Some(t) = token {
            builder = builder.header("authorization", format!("Bearer {t}"));
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn auth_allows_correct_token() {
        let app = auth_router(Some("secret-123"));
        let resp = app.oneshot(auth_request(Some("secret-123"))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_rejects_wrong_token() {
        let app = auth_router(Some("secret-123"));
        let resp = app
            .oneshot(auth_request(Some("wrong-token")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_rejects_missing_token() {
        let app = auth_router(Some("secret-123"));
        let resp = app.oneshot(auth_request(None)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_allows_any_when_disabled() {
        let app = auth_router(None);
        let resp = app.oneshot(auth_request(None)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_case_insensitive_bearer_prefix() {
        let state = Arc::new(AuthState {
            token: Some("my-token".into()),
        });
        let app = Router::new()
            .route("/test", get(ok_handler))
            .layer(middleware::from_fn_with_state(state, require_auth));

        let req = Request::builder()
            .uri("/test")
            .header("authorization", "BEARER my-token")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_rejects_non_bearer_scheme() {
        let app = auth_router(Some("secret"));
        let req = Request::builder()
            .uri("/test")
            .header("authorization", "Basic c2VjcmV0")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // ── Rate limiter middleware integration test ──────────────────────────

    #[tokio::test]
    async fn rate_limiter_returns_429_when_exhausted() {
        let limiter = Arc::new(RateLimiterState::new(2));
        let app = Router::new()
            .route("/test", get(ok_handler))
            .layer(middleware::from_fn_with_state(limiter, rate_limit));

        let app2 = app.clone();
        let app3 = app2.clone();

        let req = Request::builder().uri("/test").body(Body::empty()).unwrap();
        assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::OK);

        let req = Request::builder().uri("/test").body(Body::empty()).unwrap();
        assert_eq!(app2.oneshot(req).await.unwrap().status(), StatusCode::OK);

        let req = Request::builder().uri("/test").body(Body::empty()).unwrap();
        assert_eq!(
            app3.oneshot(req).await.unwrap().status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    // ── Combined security layer test ─────────────────────────────────────

    #[tokio::test]
    async fn combined_layers_enforce_all_guards() {
        let auth_state = Arc::new(AuthState {
            token: Some("tok-123".into()),
        });
        let limiter = Arc::new(RateLimiterState::new(100));

        let app = Router::new()
            .route("/test", get(ok_handler))
            .layer(middleware::from_fn_with_state(auth_state, require_auth))
            .layer(middleware::from_fn_with_state(limiter, rate_limit))
            .layer(middleware::from_fn(security_headers))
            .layer(middleware::from_fn(origin_guard))
            .layer(middleware::from_fn(dns_rebinding_guard));

        // Good request: all guards pass
        let req = Request::builder()
            .uri("/test")
            .header("authorization", "Bearer tok-123")
            .header("host", "127.0.0.1:7373")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("x-frame-options").unwrap(), "DENY");

        // Bad host: DNS rebinding guard blocks
        let req = Request::builder()
            .uri("/test")
            .header("authorization", "Bearer tok-123")
            .header("host", "evil.com")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // Bad origin: origin guard blocks
        let req = Request::builder()
            .uri("/test")
            .header("authorization", "Bearer tok-123")
            .header("host", "localhost")
            .header("origin", "https://evil.com")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // Missing auth: auth middleware blocks
        let req = Request::builder()
            .uri("/test")
            .header("host", "localhost")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn origin_guard_allows_localhost_variants() {
        assert!(is_allowed_origin("http://localhost"));
        assert!(is_allowed_origin("http://localhost:7373"));
        assert!(is_allowed_origin("https://localhost"));
        assert!(is_allowed_origin("https://localhost:443"));
        assert!(is_allowed_origin("http://127.0.0.1"));
        assert!(is_allowed_origin("http://127.0.0.1:8080"));
        assert!(is_allowed_origin("https://127.0.0.1"));
        assert!(is_allowed_origin("http://[::1]"));
        assert!(is_allowed_origin("http://[::1]:7373"));
        assert!(is_allowed_origin("tauri://localhost"));
        assert!(is_allowed_origin("tauri://some-app"));
    }

    #[test]
    fn origin_guard_rejects_prefix_smuggling() {
        assert!(!is_allowed_origin("http://localhost.evil.com"));
        assert!(!is_allowed_origin("https://localhost.evil.com"));
        assert!(!is_allowed_origin("https://127.0.0.1.evil.com"));
        assert!(!is_allowed_origin("http://[::1].evil.com"));
    }

    #[test]
    fn origin_guard_rejects_userinfo_trick() {
        assert!(!is_allowed_origin("http://localhost@evil.com"));
        assert!(!is_allowed_origin("http://127.0.0.1@evil.com"));
    }

    #[test]
    fn origin_guard_rejects_foreign_and_malformed() {
        assert!(!is_allowed_origin("http://evil.com"));
        assert!(!is_allowed_origin("https://attacker.io"));
        assert!(!is_allowed_origin("not-a-url"));
        assert!(!is_allowed_origin(""));
        assert!(!is_allowed_origin("ftp://localhost"));
    }
}
