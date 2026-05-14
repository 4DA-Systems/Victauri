use std::sync::Arc;

use axum::extract::DefaultBodyLimit;

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

    let info_handler_ref = handler.clone();
    let info_auth = auth_token.is_some();

    let auth_state = Arc::new(auth::AuthState {
        token: auth_token.clone(),
    });

    let mut router = axum::Router::new()
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
    router = router.layer(axum::middleware::from_fn_with_state(limiter, auth::rate_limit));

    router
        .route(
            "/health",
            axum::routing::get(|| async { axum::Json(serde_json::json!({"status": "ok"})) }),
        )
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
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
    use std::sync::Arc;

    #[test]
    fn router_builds_without_auth() {
        let tab_mgr = Arc::new(TabManager::new());
        let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        let handler = VictauriBrowserHandler::new(tab_mgr, dispatch);
        let _router = build_app(handler, None);
    }

    #[test]
    fn router_builds_with_auth() {
        let tab_mgr = Arc::new(TabManager::new());
        let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        let handler = VictauriBrowserHandler::new(tab_mgr, dispatch);
        let _router = build_app(handler, Some("test-token".to_string()));
    }
}
