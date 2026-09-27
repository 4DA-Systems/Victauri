pub use victauri_core::middleware::{
    AuthState, default_rate_limiter, dns_rebinding_guard, origin_guard, rate_limit, require_auth,
    security_headers,
};
pub use victauri_core::security::{
    self, RateLimiter as RateLimiterState, constant_time_eq, generate_token, is_allowed_origin,
    is_localhost_host,
};

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::middleware;
    use axum::routing::get;
    use tower::ServiceExt;

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
        assert_eq!(limiter.current_tokens(), 42);
        assert_eq!(limiter.max_tokens(), 42);
    }

    #[test]
    fn rate_limiter_concurrent_acquire() {
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
        assert!(
            total >= 1000,
            "should dispense at least the initial budget, got {total}"
        );
        assert!(total <= 1200, "refill overshoot too high, got {total}");
    }

    #[test]
    fn default_rate_limiter_has_expected_tokens() {
        let limiter = default_rate_limiter();
        assert_eq!(limiter.max_tokens(), 1000);
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

    fn dns_request(host: Option<&str>) -> axum::extract::Request<Body> {
        let mut builder = axum::extract::Request::builder().uri("/test");
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
    async fn dns_rebinding_blocks_empty_host() {
        let app = dns_rebinding_router();
        let resp = app.oneshot(dns_request(None)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
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

    fn origin_request(origin: Option<&str>) -> axum::extract::Request<Body> {
        let mut builder = axum::extract::Request::builder().uri("/test");
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

    /// Audit I3: `tauri://` was accepted by prefix, for any host. No caller needs it (the
    /// app's webview reaches Victauri over IPC), so it is refused.
    #[tokio::test]
    async fn origin_blocks_tauri_scheme() {
        for origin in ["tauri://localhost", "tauri://evil.com"] {
            let app = origin_router();
            let resp = app.oneshot(origin_request(Some(origin))).await.unwrap();
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{origin}");
        }
    }

    /// Audit I1: an Origin that is not visible ASCII used to be treated as ABSENT (allowed).
    #[tokio::test]
    async fn origin_blocks_unreadable_origin() {
        let app = origin_router();
        let req = axum::extract::Request::builder()
            .uri("/test")
            .header(
                "origin",
                axum::http::HeaderValue::from_bytes(b"http://\xe9vil.com").unwrap(),
            )
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.oneshot(req).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }

    /// Audit N3: a page can make a browser send `GET /health` with no Origin header
    /// (`<img src=…>`); `Sec-Fetch-Site` marks it. It is refused BEFORE the rate limiter, so it
    /// spends no token.
    #[tokio::test]
    async fn browser_initiated_requests_are_refused_without_spending_a_token() {
        let limiter = Arc::new(RateLimiterState::new(5));
        let app = Router::new()
            .route("/test", get(ok_handler))
            .layer(middleware::from_fn_with_state(limiter.clone(), rate_limit))
            .layer(middleware::from_fn(origin_guard));
        for site in ["cross-site", "same-site", "same-origin"] {
            let req = axum::extract::Request::builder()
                .uri("/test")
                .header("sec-fetch-site", site)
                .body(Body::empty())
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{site}");
        }
        assert_eq!(
            limiter.current_tokens(),
            5,
            "a refused request spent a token"
        );
        // A user navigation (`none`) and a non-browser client (no header) pass.
        let req = axum::extract::Request::builder()
            .uri("/test")
            .header("sec-fetch-site", "none")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::OK
        );
        let req = axum::extract::Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();
        assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::OK);
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
        let req = axum::extract::Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();
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
        let req = axum::extract::Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("cache-control").unwrap(), "no-store");
    }

    #[tokio::test]
    async fn security_headers_x_frame_options() {
        let app = security_headers_router();
        let req = axum::extract::Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();
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

    fn auth_request(token: Option<&str>) -> axum::extract::Request<Body> {
        let mut builder = axum::extract::Request::builder().uri("/test");
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

        let req = axum::extract::Request::builder()
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
        let req = axum::extract::Request::builder()
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

        let req = axum::extract::Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();
        assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::OK);

        let req = axum::extract::Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();
        assert_eq!(app2.oneshot(req).await.unwrap().status(), StatusCode::OK);

        let req = axum::extract::Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();
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
        let req = axum::extract::Request::builder()
            .uri("/test")
            .header("authorization", "Bearer tok-123")
            .header("host", "127.0.0.1:7373")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("x-frame-options").unwrap(), "DENY");

        // Bad host: DNS rebinding guard blocks
        let req = axum::extract::Request::builder()
            .uri("/test")
            .header("authorization", "Bearer tok-123")
            .header("host", "evil.com")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // Bad origin: origin guard blocks
        let req = axum::extract::Request::builder()
            .uri("/test")
            .header("authorization", "Bearer tok-123")
            .header("host", "localhost")
            .header("origin", "https://evil.com")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // Missing auth: auth middleware blocks
        let req = axum::extract::Request::builder()
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
        assert!(!is_allowed_origin("tauri://localhost"));
        assert!(!is_allowed_origin("tauri://some-app"));
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

    // ── Constant-time comparison tests ───────────────────────────────────

    #[test]
    fn constant_time_eq_equal_strings() {
        assert!(constant_time_eq(b"secret-token-123", b"secret-token-123"));
    }

    #[test]
    fn constant_time_eq_different_strings() {
        assert!(!constant_time_eq(b"secret-token-123", b"wrong-token-9999"));
    }

    #[test]
    fn constant_time_eq_different_lengths() {
        assert!(!constant_time_eq(b"short", b"longer-string"));
    }

    #[test]
    fn constant_time_eq_empty_strings() {
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn constant_time_eq_one_empty() {
        assert!(!constant_time_eq(b"", b"notempty"));
        assert!(!constant_time_eq(b"notempty", b""));
    }

    #[test]
    fn constant_time_eq_single_bit_difference() {
        assert!(!constant_time_eq(b"A", b"B"));
    }

    // ── Security headers: CORS + CSP tests ───────────────────────────────

    /// Audit I2: `Access-Control-Allow-Origin: null` GRANTS the opaque `null` origin
    /// (sandboxed iframes, `data:` documents); denying CORS means sending no ACAO at all.
    #[tokio::test]
    async fn security_headers_cors_deny() {
        let app = security_headers_router();
        let req = axum::extract::Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert!(resp.headers().get("access-control-allow-origin").is_none());
    }

    #[tokio::test]
    async fn security_headers_csp() {
        let app = security_headers_router();
        let req = axum::extract::Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.headers().get("content-security-policy").unwrap(),
            "default-src 'none'"
        );
    }
}
