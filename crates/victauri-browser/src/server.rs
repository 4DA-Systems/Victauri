use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

use crate::auth;
use crate::mcp_handler::VictauriBrowserHandler;

/// Build the axum router for the browser MCP server.
///
/// Mirrors `victauri-plugin`'s server pattern: `/mcp` for MCP Streamable HTTP,
/// `/api/tools` for REST, `/health` and `/info` for diagnostics.
pub fn build_app(handler: VictauriBrowserHandler, auth_token: Option<String>) -> axum::Router {
    build_app_full(handler, auth_token, None)
}

/// Build the axum router with full control over auth and rate limiting.
pub fn build_app_full(
    handler: VictauriBrowserHandler,
    auth_token: Option<String>,
    rate_limiter: Option<Arc<auth::RateLimiterState>>,
) -> axum::Router {
    let rest = rest_routes(handler.clone());

    let mcp_handler = handler.clone();
    let mcp_service = StreamableHttpService::new(
        move || Ok(mcp_handler.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );

    let info_handler_ref = handler.clone();
    let info_auth = auth_token.is_some();

    let auth_state = Arc::new(auth::AuthState {
        token: auth_token.clone(),
    });

    let mut router = axum::Router::new()
        .route_service("/mcp", mcp_service)
        .nest("/api/tools", rest)
        .route(
            "/info",
            axum::routing::get(move || {
                let h = info_handler_ref.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "name": "victauri-browser",
                        "version": env!("CARGO_PKG_VERSION"),
                        "protocol": "mcp",
                        "mode": "browser",
                        "tabs": h.tab_count().await,
                        "auth_required": info_auth,
                    }))
                }
            }),
        );

    if auth_token.is_some() {
        router = router.layer(axum::middleware::from_fn_with_state(
            auth_state,
            auth::require_auth,
        ));
    }

    let limiter = rate_limiter.unwrap_or_else(auth::default_rate_limiter);
    router = router.layer(axum::middleware::from_fn_with_state(
        limiter,
        auth::rate_limit,
    ));

    router
        .route(
            "/health",
            axum::routing::get(|| async { axum::Json(serde_json::json!({"status": "ok"})) }),
        )
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        // Bound concurrent in-flight requests so a flood can't exhaust memory/tasks
        // (audit B9). Mirrors victauri-plugin's ConcurrencyLimitLayer::new(64).
        .layer(tower::limit::ConcurrencyLimitLayer::new(64))
        .layer(axum::middleware::from_fn(auth::security_headers))
        .layer(axum::middleware::from_fn(auth::origin_guard))
        .layer(axum::middleware::from_fn(auth::dns_rebinding_guard))
}

fn rest_routes(handler: VictauriBrowserHandler) -> axum::Router {
    let list_handler = handler.clone();

    axum::Router::new()
        .route(
            "/",
            axum::routing::get(move || {
                let h = list_handler.clone();
                async move { axum::Json(h.list_tools()) }
            }),
        )
        .route(
            "/{name}",
            axum::routing::post(move |path, body| execute_tool(handler, path, body)),
        )
}

async fn execute_tool(
    handler: VictauriBrowserHandler,
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::Json(args): axum::Json<serde_json::Value>,
) -> axum::Json<serde_json::Value> {
    match handler.execute_tool(&name, args).await {
        Ok(result) => axum::Json(serde_json::json!({"result": result})),
        Err(e) => axum::Json(serde_json::json!({"error": e})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge_dispatch::BridgeDispatch;
    use crate::tab_state::TabManager;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn make_app(auth: Option<String>) -> axum::Router {
        let tab_mgr = Arc::new(TabManager::new());
        let dispatch = Arc::new(BridgeDispatch::new_sink());
        let handler = VictauriBrowserHandler::new(tab_mgr, dispatch);
        build_app(handler, auth)
    }

    fn req(uri: &str) -> axum::http::request::Builder {
        axum::http::Request::builder()
            .uri(uri)
            .header("host", "localhost")
    }

    async fn get_json(
        app: axum::Router,
        path: &str,
    ) -> (axum::http::StatusCode, serde_json::Value) {
        let req = axum::http::Request::builder()
            .uri(path)
            .header("host", "localhost")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        (status, json)
    }

    async fn post_json(
        app: axum::Router,
        path: &str,
        body: serde_json::Value,
        auth: Option<&str>,
    ) -> (axum::http::StatusCode, serde_json::Value) {
        let mut req = axum::http::Request::builder()
            .method("POST")
            .uri(path)
            .header("host", "localhost")
            .header("content-type", "application/json");
        if let Some(token) = auth {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        let req = req
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        (status, json)
    }

    #[test]
    fn router_builds_without_auth() {
        let _router = make_app(None);
    }

    #[test]
    fn router_builds_with_auth() {
        let _router = make_app(Some("test-token".to_string()));
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let (status, json) = get_json(make_app(None), "/health").await;
        assert_eq!(status, 200);
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn info_returns_metadata() {
        let (status, json) = get_json(make_app(None), "/info").await;
        assert_eq!(status, 200);
        assert_eq!(json["name"], "victauri-browser");
        assert_eq!(json["protocol"], "mcp");
        assert_eq!(json["mode"], "browser");
        assert_eq!(json["tabs"], 0);
    }

    #[tokio::test]
    async fn tool_list_returns_20() {
        let (status, json) = get_json(make_app(None), "/api/tools").await;
        assert_eq!(status, 200);
        assert_eq!(json.as_array().unwrap().len(), 20);
    }

    #[tokio::test]
    async fn plugin_info_via_rest() {
        let (status, json) = post_json(
            make_app(None),
            "/api/tools/get_plugin_info",
            serde_json::json!({}),
            None,
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(json["result"]["name"], "victauri-browser");
        assert_eq!(json["result"]["tool_count"], 20);
    }

    #[tokio::test]
    async fn tabs_list_empty_via_rest() {
        let (status, json) = post_json(
            make_app(None),
            "/api/tools/tabs",
            serde_json::json!({"action": "list"}),
            None,
        )
        .await;
        assert_eq!(status, 200);
        assert!(json["result"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unknown_tool_returns_error() {
        let (status, json) = post_json(
            make_app(None),
            "/api/tools/nonexistent",
            serde_json::json!({}),
            None,
        )
        .await;
        assert_eq!(status, 200);
        assert!(json["error"].as_str().unwrap().contains("unknown tool"));
    }

    #[tokio::test]
    async fn auth_blocks_without_token() {
        let app = make_app(Some("secret-token".to_string()));
        let (status, _) = get_json(app, "/info").await;
        assert_eq!(status, 401);
    }

    #[tokio::test]
    async fn auth_passes_with_correct_token() {
        let token = "secret-token";
        let app = make_app(Some(token.to_string()));
        let req = req("/info")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn health_bypasses_auth() {
        let app = make_app(Some("secret-token".to_string()));
        let (status, json) = get_json(app, "/health").await;
        assert_eq!(status, 200);
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn security_headers_present() {
        let app = make_app(None);
        let req = req("/health").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
        assert_eq!(resp.headers().get("cache-control").unwrap(), "no-store");
    }

    #[tokio::test]
    async fn non_local_origin_blocked() {
        let app = make_app(None);
        let req = req("/health")
            .header("origin", "https://evil.com")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn local_origin_allowed() {
        let app = make_app(None);
        let req = req("/health")
            .header("origin", "http://127.0.0.1:7474")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    fn make_app_with_rate_limit(budget: u64) -> axum::Router {
        let tab_mgr = Arc::new(TabManager::new());
        let dispatch = Arc::new(BridgeDispatch::new_sink());
        let handler = VictauriBrowserHandler::new(tab_mgr, dispatch);
        let limiter = Arc::new(crate::auth::RateLimiterState::new(budget));
        build_app_full(handler, None, Some(limiter))
    }

    #[tokio::test]
    async fn rate_limit_exhaustion_returns_429() {
        let app = make_app_with_rate_limit(1);

        let req1 = req("/info").body(Body::empty()).unwrap();
        let resp1 = app.clone().oneshot(req1).await.unwrap();
        assert_eq!(resp1.status(), 200);

        let req2 = req("/info").body(Body::empty()).unwrap();
        let resp2 = app.oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), 429);
    }

    #[tokio::test]
    async fn auth_wrong_token_returns_401() {
        let app = make_app(Some("correct-token".to_string()));
        let req = req("/info")
            .header("authorization", "Bearer wrong-token")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn auth_no_bearer_prefix_returns_401() {
        let app = make_app(Some("my-token".to_string()));
        let req = req("/info")
            .header("authorization", "my-token")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn auth_case_insensitive_bearer() {
        let token = "my-secret-token";
        let app = make_app(Some(token.to_string()));
        let req = req("/info")
            .header("authorization", format!("BEARER {token}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn rest_tool_with_auth() {
        let token = "secret";
        let (status, json) = post_json(
            make_app(Some(token.to_string())),
            "/api/tools/get_plugin_info",
            serde_json::json!({}),
            Some(token),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(json["result"]["name"], "victauri-browser");
    }

    #[tokio::test]
    async fn rest_tool_without_auth_when_required() {
        let (status, _) = post_json(
            make_app(Some("secret".to_string())),
            "/api/tools/get_plugin_info",
            serde_json::json!({}),
            None,
        )
        .await;
        assert_eq!(status, 401);
    }

    #[tokio::test]
    async fn localhost_origin_allowed() {
        let app = make_app(None);
        let req = req("/health")
            .header("origin", "http://localhost:3000")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn ipv6_localhost_origin_allowed() {
        let app = make_app(None);
        let req = req("/health")
            .header("origin", "http://[::1]:7474")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn no_origin_header_allowed() {
        let app = make_app(None);
        let (status, json) = get_json(app, "/health").await;
        assert_eq!(status, 200);
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn info_shows_auth_required() {
        let (_, json_no_auth) = get_json(make_app(None), "/info").await;
        assert_eq!(json_no_auth["auth_required"], false);

        let app = make_app(Some("tok".to_string()));
        let req = req("/info")
            .header("authorization", "Bearer tok")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json_auth: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json_auth["auth_required"], true);
    }

    #[tokio::test]
    async fn tool_list_via_rest_has_names() {
        let (_, json) = get_json(make_app(None), "/api/tools").await;
        let tools = json.as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"eval_js"));
        assert!(names.contains(&"dom_snapshot"));
        assert!(names.contains(&"screenshot"));
        assert!(names.contains(&"tabs"));
    }

    #[tokio::test]
    async fn rest_error_format() {
        let (status, json) = post_json(
            make_app(None),
            "/api/tools/nonexistent",
            serde_json::json!({}),
            None,
        )
        .await;
        assert_eq!(status, 200);
        assert!(json.get("error").is_some());
        assert!(json.get("result").is_none());
    }

    #[tokio::test]
    async fn rest_success_format() {
        let (status, json) = post_json(
            make_app(None),
            "/api/tools/get_plugin_info",
            serde_json::json!({}),
            None,
        )
        .await;
        assert_eq!(status, 200);
        assert!(json.get("result").is_some());
        assert!(json.get("error").is_none());
    }

    // --- Adversarial stress tests ---

    // SECURITY: Origin guard bypass attempts — all must be BLOCKED
    #[tokio::test]
    async fn origin_bypass_localhost_in_subdomain_blocked() {
        let app = make_app(None);
        let req = req("/health")
            .header("origin", "https://localhost.evil.com")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn origin_bypass_127_in_subdomain_blocked() {
        let app = make_app(None);
        let req = req("/health")
            .header("origin", "https://127.0.0.1.evil.com")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn origin_with_path_containing_localhost_blocked() {
        let app = make_app(None);
        let req = req("/health")
            .header("origin", "https://evil.com/localhost")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn origin_evil_localhost_prefix_blocked() {
        let app = make_app(None);
        let req = req("/health")
            .header("origin", "https://evil-localhost.com")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn origin_localhost_with_port_allowed() {
        let app = make_app(None);
        let req = req("/health")
            .header("origin", "http://localhost:9999")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn origin_127_with_port_allowed() {
        let app = make_app(None);
        let req = req("/health")
            .header("origin", "http://127.0.0.1:8080")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn auth_with_extra_spaces_in_bearer() {
        let token = "test-token";
        let app = make_app(Some(token.to_string()));
        let req = req("/info")
            .header("authorization", "Bearer  test-token")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        // Extra space becomes part of the token — should reject
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn auth_with_trailing_whitespace() {
        let token = "test-token";
        let app = make_app(Some(token.to_string()));
        let req = req("/info")
            .header("authorization", "Bearer test-token ")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        // Trailing space: "test-token " != "test-token"
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn auth_empty_bearer_token() {
        let token = "secret";
        let app = make_app(Some(token.to_string()));
        let req = req("/info")
            .header("authorization", "Bearer ")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn rate_limit_concurrent_burst() {
        let app = make_app_with_rate_limit(5);

        let mut handles = vec![];
        for _ in 0..20 {
            let a = app.clone();
            handles.push(tokio::spawn(async move {
                let req = req("/info").body(Body::empty()).unwrap();
                let resp = a.oneshot(req).await.unwrap();
                resp.status()
            }));
        }

        let mut ok_count = 0u32;
        let mut limited_count = 0u32;
        for h in handles {
            match h.await.unwrap().as_u16() {
                200 => ok_count += 1,
                429 => limited_count += 1,
                s => panic!("unexpected status: {s}"),
            }
        }

        assert!(ok_count <= 5, "too many passed: {ok_count}");
        assert!(ok_count >= 1, "none passed");
        assert!(limited_count >= 15, "not enough limited: {limited_count}");
    }

    #[tokio::test]
    async fn body_limit_enforcement() {
        let app = make_app(None);
        let huge_body = "x".repeat(3 * 1024 * 1024);
        let req = req("/api/tools/eval_js")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(huge_body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 413);
    }

    #[tokio::test]
    async fn malformed_json_body() {
        let app = make_app(None);
        let req = req("/api/tools/eval_js")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from("not json {{{"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        // axum returns 400 Bad Request for malformed JSON
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn empty_body_on_post() {
        let app = make_app(None);
        let req = req("/api/tools/eval_js")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        // axum returns 400 for empty body (can't parse as JSON)
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn very_long_tool_name_in_path() {
        let long_name = "x".repeat(10_000);
        let (status, json) = post_json(
            make_app(None),
            &format!("/api/tools/{long_name}"),
            serde_json::json!({}),
            None,
        )
        .await;
        assert_eq!(status, 200);
        assert!(json["error"].as_str().unwrap().contains("unknown tool"));
    }

    #[tokio::test]
    async fn tool_name_with_path_traversal() {
        let (status, json) = post_json(
            make_app(None),
            "/api/tools/../../../etc/passwd",
            serde_json::json!({}),
            None,
        )
        .await;
        // axum normalizes paths, so this should be a 404 or match a different route
        assert!(status == 200 || status == 404);
        if status == 200 {
            assert!(json.get("error").is_some());
        }
    }

    #[tokio::test]
    async fn security_headers_on_all_responses() {
        let app = make_app(None);

        for path in ["/health", "/info", "/api/tools"] {
            let req = req(path).body(Body::empty()).unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(
                resp.headers().get("x-content-type-options").unwrap(),
                "nosniff",
                "missing header on {path}"
            );
            assert_eq!(
                resp.headers().get("cache-control").unwrap(),
                "no-store",
                "missing cache header on {path}"
            );
        }
    }

    #[tokio::test]
    async fn concurrent_health_checks_100() {
        let app = make_app(None);
        let mut handles = vec![];

        for _ in 0..100 {
            let a = app.clone();
            handles.push(tokio::spawn(async move {
                let req = req("/health").body(Body::empty()).unwrap();
                a.oneshot(req).await.unwrap().status()
            }));
        }

        for h in handles {
            assert_eq!(h.await.unwrap(), 200);
        }
    }

    #[tokio::test]
    async fn method_not_allowed_on_get_to_tool() {
        let app = make_app(None);
        let req = req("/api/tools/eval_js")
            .method("GET")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 405);
    }

    #[tokio::test]
    async fn put_method_on_health() {
        let app = make_app(None);
        let req = req("/health").method("PUT").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 405);
    }

    #[tokio::test]
    async fn nonexistent_path_returns_404() {
        let app = make_app(None);
        let req = req("/nonexistent").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 404);
    }

    // --- Deep challenger tests ---

    #[tokio::test]
    async fn auth_token_with_unicode_rejected() {
        let token = "valid-token";
        let app = make_app(Some(token.to_string()));
        let req = req("/info")
            .header("authorization", "Bearer valid-token\u{200B}")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        // Zero-width space appended — must reject
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn auth_token_with_newline_rejected() {
        let token = "valid-token";
        let app = make_app(Some(token.to_string()));
        // Newline in token value — a header injection attempt
        let req = req("/info")
            .header("authorization", "Bearer valid-token\r\nX-Evil: injected")
            .body(Body::empty());
        if let Ok(req) = req {
            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), 401);
        }
        // If Err: http crate rejects headers with CRLF — attack blocked at protocol level
    }

    #[tokio::test]
    async fn content_type_missing_on_post_tool() {
        let app = make_app(None);
        let req = req("/api/tools/get_plugin_info")
            .method("POST")
            .body(Body::from("{}"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        // axum requires content-type for JSON extraction — 415 Unsupported Media Type
        assert!(
            resp.status() == 415 || resp.status() == 400,
            "expected 415 or 400, got {}",
            resp.status()
        );
    }

    #[tokio::test]
    async fn content_type_wrong_on_post_tool() {
        let app = make_app(None);
        let req = req("/api/tools/get_plugin_info")
            .method("POST")
            .header("content-type", "text/plain")
            .body(Body::from("{}"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert!(
            resp.status() == 415 || resp.status() == 400,
            "expected 415 or 400, got {}",
            resp.status()
        );
    }

    #[tokio::test]
    async fn concurrent_tool_calls_all_return() {
        let app = make_app(None);
        let mut handles = vec![];
        for _ in 0..50 {
            let a = app.clone();
            handles.push(tokio::spawn(async move {
                let req = req("/api/tools/get_plugin_info")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap();
                let resp = a.oneshot(req).await.unwrap();
                let status = resp.status();
                let body = resp.into_body().collect().await.unwrap().to_bytes();
                let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
                (status, json)
            }));
        }
        for h in handles {
            let (status, json) = h.await.unwrap();
            assert_eq!(status, 200);
            assert_eq!(json["result"]["name"], "victauri-browser");
        }
    }

    #[tokio::test]
    async fn tool_name_with_url_encoded_chars() {
        let app = make_app(None);
        // %5F = underscore. "get%5Fplugin%5Finfo" → "get_plugin_info"
        let req = req("/api/tools/get%5Fplugin%5Finfo")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, 200);
        // Should route to get_plugin_info (URL decoded)
        assert_eq!(json["result"]["name"], "victauri-browser");
    }

    #[tokio::test]
    async fn auth_token_timing_attack_resistance() {
        // Verify that wrong tokens of different lengths both fail
        let token = "a".repeat(64);
        let app = make_app(Some(token.clone()));

        // Wrong token same length
        let wrong_same_len = "b".repeat(64);
        let r = req("/info")
            .header("authorization", format!("Bearer {wrong_same_len}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(r).await.unwrap();
        assert_eq!(resp.status(), 401);

        // Wrong token different length
        let r = req("/info")
            .header("authorization", "Bearer short")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(r).await.unwrap();
        assert_eq!(resp.status(), 401);

        // Correct token works
        let r = req("/info")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(r).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn origin_with_credentials_in_url() {
        let app = make_app(None);
        let req = req("/health")
            .header("origin", "http://user:pass@evil.com")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn origin_localhost_with_credentials() {
        let app = make_app(None);
        let req = req("/health")
            .header("origin", "http://user:pass@localhost:7474")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        // URL with credentials: "user:pass@localhost" — after stripping scheme,
        // the host extraction splits on ':' and gets "user" (not "localhost")
        // This is actually a legitimate security concern — should be blocked
        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn multiple_authorization_headers() {
        let token = "real-token";
        let app = make_app(Some(token.to_string()));
        // HTTP allows multiple header values; axum takes the first
        let req = req("/info")
            .header("authorization", "Bearer wrong-token")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        // Should reject — first header wins, and it's wrong
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn json_body_with_duplicate_keys() {
        let app = make_app(None);
        // JSON with duplicate "action" keys — serde takes the last one
        let req = req("/api/tools/tabs")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"action": "get_state", "action": "list"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, 200);
        // serde_json takes the last key, so action="list" wins
        assert!(json["result"].is_array());
    }

    #[tokio::test]
    async fn very_large_json_within_limit() {
        let app = make_app(None);
        // 1.5MB of JSON body — under the 2MB limit
        // Use get_plugin_info which doesn't dispatch to bridge (avoids 30s timeout)
        let padding = "x".repeat(1_500_000);
        let body = serde_json::json!({"unused_field": padding});
        let req = req("/api/tools/get_plugin_info")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["result"]["name"], "victauri-browser");
    }

    #[tokio::test]
    async fn head_request_on_health() {
        let app = make_app(None);
        let req = req("/health").method("HEAD").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        // HEAD on a GET route should return 200 with no body
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn options_request_on_tool() {
        let app = make_app(None);
        let req = req("/api/tools/eval_js")
            .method("OPTIONS")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        // OPTIONS typically returns 200 or 405 depending on CORS config
        assert!(resp.status() == 200 || resp.status() == 405);
    }

    #[tokio::test]
    async fn rapid_auth_failures_dont_leak_info() {
        let token = "secret-token-value";
        let app = make_app(Some(token.to_string()));

        for attempt in [
            "",
            "wrong",
            "secret-token-valu",
            "secret-token-value!",
            &"x".repeat(1000),
        ] {
            let req = req("/info")
                .header("authorization", format!("Bearer {attempt}"))
                .body(Body::empty())
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), 401, "should reject: {attempt:?}");
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            // Body should not contain the actual token or any hint
            let body_str = String::from_utf8_lossy(&body);
            assert!(!body_str.contains(token), "response leaked the token");
        }
    }
}
