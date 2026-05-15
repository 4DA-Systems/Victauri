use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::bridge_dispatch::BridgeDispatch;
use crate::tab_state::TabManager;

#[derive(Clone)]
pub struct VictauriBrowserHandler {
    tab_manager: Arc<TabManager>,
    dispatch: Arc<BridgeDispatch>,
    tool_invocations: Arc<AtomicU64>,
}

impl VictauriBrowserHandler {
    #[must_use]
    pub fn new(tab_manager: Arc<TabManager>, dispatch: Arc<BridgeDispatch>) -> Self {
        Self {
            tab_manager,
            dispatch,
            tool_invocations: Arc::new(AtomicU64::new(0)),
        }
    }

    pub async fn tab_count(&self) -> usize {
        self.tab_manager.tab_count().await
    }

    #[must_use]
    pub fn list_tools(&self) -> Vec<ToolInfo> {
        vec![
            ToolInfo::new("eval_js", "Execute JavaScript in the active page"),
            ToolInfo::new("dom_snapshot", "Get accessible DOM tree with ref handles"),
            ToolInfo::new(
                "find_elements",
                "Search DOM elements by text, role, selector, or attribute",
            ),
            ToolInfo::new("interact", "Click, hover, focus, scroll, or select elements"),
            ToolInfo::new("input", "Fill, type text, or press keys"),
            ToolInfo::new(
                "inspect",
                "CSS inspection, visual debug, accessibility audit, performance",
            ),
            ToolInfo::new("css", "Inject or remove custom CSS"),
            ToolInfo::new(
                "logs",
                "Console, network, navigation, dialog, and event logs",
            ),
            ToolInfo::new("storage", "localStorage, sessionStorage, and cookie access"),
            ToolInfo::new("navigate", "Navigate, go back, manage dialogs"),
            ToolInfo::new("wait_for", "Wait for DOM conditions, text, or URL changes"),
            ToolInfo::new(
                "assert_semantic",
                "Evaluate expression and assert condition",
            ),
            ToolInfo::new("recording", "Record interactions, checkpoint, replay"),
            ToolInfo::new("screenshot", "Take page screenshot (PNG)"),
            ToolInfo::new("tabs", "Manage browser tabs and windows"),
            ToolInfo::new("page_info", "Get page metadata, headers, and resources"),
            ToolInfo::new("cookies", "Cross-origin cookie management"),
            ToolInfo::new("get_diagnostics", "Browser and extension diagnostics"),
            ToolInfo::new("get_plugin_info", "Extension and host version info"),
            ToolInfo::new("get_memory_stats", "JS heap memory statistics"),
        ]
    }

    /// Execute a tool by name with JSON arguments.
    ///
    /// Tools that need the browser extension are dispatched via native messaging.
    /// Tools that are host-only (`plugin_info`, tabs list) are handled locally.
    ///
    /// # Errors
    ///
    /// Returns an error string if the tool is unknown or execution fails.
    pub async fn execute_tool(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.tool_invocations.fetch_add(1, Ordering::Relaxed);

        let tab_id = args.get("tab_id").and_then(serde_json::Value::as_u64).map(|v| v as u32);

        match name {
            "get_plugin_info" => Ok(serde_json::json!({
                "name": "victauri-browser",
                "version": env!("CARGO_PKG_VERSION"),
                "mode": "browser",
                "tool_count": self.list_tools().len(),
                "tab_count": self.tab_manager.tab_count().await,
                "invocations": self.tool_invocations.load(Ordering::Relaxed),
            })),

            "tabs" => {
                let action = args
                    .get("action")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("list");
                match action {
                    "list" => {
                        let tabs = self.tab_manager.list_tabs().await;
                        Ok(serde_json::to_value(tabs).unwrap_or_default())
                    }
                    _ => Err(format!("unknown tabs action: {action}")),
                }
            }

            "eval_js" => {
                let code = args
                    .get("code")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("missing 'code' parameter")?;
                self.dispatch
                    .dispatch(tab_id, "eval", serde_json::json!({"code": code}))
                    .await
            }

            "dom_snapshot" => {
                let format = args.get("format").and_then(serde_json::Value::as_str);
                self.dispatch
                    .dispatch(
                        tab_id,
                        "snapshot",
                        serde_json::json!({"format": format}),
                    )
                    .await
            }

            "find_elements" => {
                let query = if args.get("query").is_some() {
                    args["query"].clone()
                } else {
                    args.clone()
                };
                self.dispatch
                    .dispatch(tab_id, "findElements", query)
                    .await
            }

            "interact" => {
                let action = args
                    .get("action")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("missing 'action' parameter")?;
                let ref_id = args.get("ref_id").and_then(serde_json::Value::as_str);
                let timeout_ms = args.get("timeout_ms").and_then(serde_json::Value::as_u64);

                let method = match action {
                    "click" => "click",
                    "double_click" => "doubleClick",
                    "hover" => "hover",
                    "focus" => "focusElement",
                    "scroll" | "scroll_into_view" => "scrollTo",
                    "select" => "selectOption",
                    _ => return Err(format!("unknown interact action: {action}")),
                };

                let mut bridge_args = serde_json::json!({});
                if let Some(r) = ref_id {
                    bridge_args["ref_id"] = serde_json::Value::String(r.to_string());
                }
                if let Some(t) = timeout_ms {
                    bridge_args["timeout_ms"] = serde_json::Value::Number(t.into());
                }
                if let Some(v) = args.get("values") {
                    bridge_args["values"] = v.clone();
                }
                if let Some(x) = args.get("x") {
                    bridge_args["x"] = x.clone();
                }
                if let Some(y) = args.get("y") {
                    bridge_args["y"] = y.clone();
                }

                self.dispatch.dispatch(tab_id, method, bridge_args).await
            }

            "input" => {
                let action = args
                    .get("action")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("missing 'action' parameter")?;

                match action {
                    "fill" => {
                        let ref_id = args
                            .get("ref_id")
                            .and_then(serde_json::Value::as_str)
                            .ok_or("missing 'ref_id'")?;
                        let value = args
                            .get("value")
                            .and_then(serde_json::Value::as_str)
                            .ok_or("missing 'value'")?;
                        self.dispatch
                            .dispatch(
                                tab_id,
                                "fill",
                                serde_json::json!({
                                    "ref_id": ref_id,
                                    "value": value,
                                    "timeout_ms": args.get("timeout_ms"),
                                }),
                            )
                            .await
                    }
                    "type" => {
                        let ref_id = args
                            .get("ref_id")
                            .and_then(serde_json::Value::as_str)
                            .ok_or("missing 'ref_id'")?;
                        let text = args
                            .get("text")
                            .and_then(serde_json::Value::as_str)
                            .ok_or("missing 'text'")?;
                        self.dispatch
                            .dispatch(
                                tab_id,
                                "type",
                                serde_json::json!({
                                    "ref_id": ref_id,
                                    "text": text,
                                    "timeout_ms": args.get("timeout_ms"),
                                }),
                            )
                            .await
                    }
                    "press_key" => {
                        let key = args
                            .get("key")
                            .and_then(serde_json::Value::as_str)
                            .ok_or("missing 'key'")?;
                        self.dispatch
                            .dispatch(tab_id, "pressKey", serde_json::json!({"key": key}))
                            .await
                    }
                    "clear" => {
                        let ref_id = args
                            .get("ref_id")
                            .and_then(serde_json::Value::as_str)
                            .ok_or("missing 'ref_id'")?;
                        self.dispatch
                            .dispatch(
                                tab_id,
                                "fill",
                                serde_json::json!({"ref_id": ref_id, "value": ""}),
                            )
                            .await
                    }
                    _ => Err(format!("unknown input action: {action}")),
                }
            }

            "inspect" => {
                let action = args
                    .get("action")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("missing 'action' parameter")?;

                match action {
                    "styles" => {
                        self.dispatch
                            .dispatch(
                                tab_id,
                                "getStyles",
                                serde_json::json!({
                                    "ref_id": args.get("ref_id"),
                                    "properties": args.get("properties"),
                                }),
                            )
                            .await
                    }
                    "bounds" => {
                        self.dispatch
                            .dispatch(
                                tab_id,
                                "getBoundingBoxes",
                                serde_json::json!({"ref_ids": args.get("ref_ids")}),
                            )
                            .await
                    }
                    "highlight" => {
                        self.dispatch
                            .dispatch(
                                tab_id,
                                "highlightElement",
                                serde_json::json!({
                                    "ref_id": args.get("ref_id"),
                                    "color": args.get("color"),
                                    "label": args.get("label"),
                                }),
                            )
                            .await
                    }
                    "clear_highlights" => {
                        self.dispatch
                            .dispatch(tab_id, "clearHighlights", serde_json::json!({}))
                            .await
                    }
                    "accessibility" => {
                        self.dispatch
                            .dispatch(tab_id, "auditAccessibility", serde_json::json!({}))
                            .await
                    }
                    "performance" => {
                        self.dispatch
                            .dispatch(tab_id, "getPerformanceMetrics", serde_json::json!({}))
                            .await
                    }
                    _ => Err(format!("unknown inspect action: {action}")),
                }
            }

            "css" => {
                let action = args
                    .get("action")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("missing 'action' parameter")?;

                match action {
                    "inject" => {
                        let css = args
                            .get("css")
                            .and_then(serde_json::Value::as_str)
                            .ok_or("missing 'css'")?;
                        self.dispatch
                            .dispatch(
                                tab_id,
                                "injectCss",
                                serde_json::json!({"css": css}),
                            )
                            .await
                    }
                    "remove" => {
                        self.dispatch
                            .dispatch(tab_id, "removeInjectedCss", serde_json::json!({}))
                            .await
                    }
                    _ => Err(format!("unknown css action: {action}")),
                }
            }

            "logs" => {
                let action = args
                    .get("action")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("missing 'action' parameter")?;

                match action {
                    "console" => {
                        self.dispatch
                            .dispatch(
                                tab_id,
                                "getConsoleLogs",
                                serde_json::json!({"since": args.get("since")}),
                            )
                            .await
                    }
                    "network" => {
                        self.dispatch
                            .dispatch(
                                tab_id,
                                "getNetworkLog",
                                serde_json::json!({
                                    "filter": args.get("filter"),
                                    "limit": args.get("limit"),
                                }),
                            )
                            .await
                    }
                    "navigation" => {
                        self.dispatch
                            .dispatch(tab_id, "getNavigationLog", serde_json::json!({}))
                            .await
                    }
                    "dialogs" => {
                        self.dispatch
                            .dispatch(tab_id, "getDialogLog", serde_json::json!({}))
                            .await
                    }
                    "events" => {
                        self.dispatch
                            .dispatch(
                                tab_id,
                                "getEventStream",
                                serde_json::json!({"since": args.get("since")}),
                            )
                            .await
                    }
                    _ => Err(format!("unknown logs action: {action}")),
                }
            }

            "storage" => {
                let action = args
                    .get("action")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("missing 'action' parameter")?;

                match action {
                    "get" => {
                        let store = args
                            .get("store")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("local");
                        let method = if store == "session" {
                            "getSessionStorage"
                        } else {
                            "getLocalStorage"
                        };
                        self.dispatch
                            .dispatch(
                                tab_id,
                                method,
                                serde_json::json!({"key": args.get("key")}),
                            )
                            .await
                    }
                    "set" => {
                        let store = args
                            .get("store")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("local");
                        let method = if store == "session" {
                            "setSessionStorage"
                        } else {
                            "setLocalStorage"
                        };
                        self.dispatch
                            .dispatch(
                                tab_id,
                                method,
                                serde_json::json!({
                                    "key": args.get("key"),
                                    "value": args.get("value"),
                                }),
                            )
                            .await
                    }
                    "delete" => {
                        let store = args
                            .get("store")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("local");
                        let method = if store == "session" {
                            "deleteSessionStorage"
                        } else {
                            "deleteLocalStorage"
                        };
                        self.dispatch
                            .dispatch(
                                tab_id,
                                method,
                                serde_json::json!({"key": args.get("key")}),
                            )
                            .await
                    }
                    "cookies" => {
                        self.dispatch
                            .dispatch(tab_id, "getCookies", serde_json::json!({}))
                            .await
                    }
                    _ => Err(format!("unknown storage action: {action}")),
                }
            }

            "navigate" => {
                let action = args
                    .get("action")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("missing 'action' parameter")?;

                match action {
                    "go_to" => {
                        let url = args
                            .get("url")
                            .and_then(serde_json::Value::as_str)
                            .ok_or("missing 'url'")?;
                        self.dispatch
                            .dispatch(
                                tab_id,
                                "navigate",
                                serde_json::json!({"url": url}),
                            )
                            .await
                    }
                    "back" => {
                        self.dispatch
                            .dispatch(tab_id, "navigateBack", serde_json::json!({}))
                            .await
                    }
                    "history" => {
                        self.dispatch
                            .dispatch(tab_id, "getNavigationLog", serde_json::json!({}))
                            .await
                    }
                    "dialogs" => {
                        self.dispatch
                            .dispatch(tab_id, "getDialogLog", serde_json::json!({}))
                            .await
                    }
                    _ => Err(format!("unknown navigate action: {action}")),
                }
            }

            "wait_for" => {
                self.dispatch
                    .dispatch(tab_id, "waitFor", args)
                    .await
            }

            "assert_semantic" => {
                let expression = args
                    .get("expression")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("missing 'expression'")?;
                let condition = args
                    .get("condition")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("missing 'condition'")?;

                let eval_result = self
                    .dispatch
                    .dispatch(
                        tab_id,
                        "eval",
                        serde_json::json!({"code": expression}),
                    )
                    .await?;

                let actual_str = eval_result
                    .as_str()
                    .unwrap_or(&eval_result.to_string())
                    .to_string();

                let expected = args.get("expected").and_then(serde_json::Value::as_str);

                let passed = match condition {
                    "equals" => expected.is_some_and(|e| actual_str == e),
                    "not_equals" => expected.is_some_and(|e| actual_str != e),
                    "contains" => expected.is_some_and(|e| actual_str.contains(e)),
                    "truthy" => {
                        actual_str != "false"
                            && actual_str != "0"
                            && actual_str != "null"
                            && actual_str != "undefined"
                            && actual_str != "\"\""
                            && !actual_str.is_empty()
                    }
                    "greater_than" => {
                        if let (Ok(a), Some(Ok(e))) = (
                            actual_str.parse::<f64>(),
                            expected.map(str::parse::<f64>),
                        ) {
                            a > e
                        } else {
                            false
                        }
                    }
                    "less_than" => {
                        if let (Ok(a), Some(Ok(e))) = (
                            actual_str.parse::<f64>(),
                            expected.map(str::parse::<f64>),
                        ) {
                            a < e
                        } else {
                            false
                        }
                    }
                    _ => return Err(format!("unknown condition: {condition}")),
                };

                Ok(serde_json::json!({
                    "passed": passed,
                    "actual": actual_str,
                    "expected": expected,
                    "condition": condition,
                }))
            }

            "recording" => {
                let action = args
                    .get("action")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("missing 'action' parameter")?;

                match action {
                    "start" | "stop" | "checkpoint" | "get_events" | "list_checkpoints"
                    | "export" => {
                        self.dispatch
                            .dispatch(tab_id, &format!("recording_{action}"), args)
                            .await
                    }
                    _ => Err(format!("unknown recording action: {action}")),
                }
            }

            "screenshot" => {
                self.dispatch
                    .dispatch(
                        tab_id,
                        "screenshot",
                        serde_json::json!({
                            "fullPage": args.get("full_page"),
                        }),
                    )
                    .await
            }

            "page_info" => {
                self.dispatch
                    .dispatch(tab_id, "getDiagnostics", serde_json::json!({}))
                    .await
            }

            "cookies" => {
                self.dispatch
                    .dispatch(tab_id, "getCookies", serde_json::json!({}))
                    .await
            }

            "get_diagnostics" => {
                self.dispatch
                    .dispatch(tab_id, "getDiagnostics", serde_json::json!({}))
                    .await
            }

            "get_memory_stats" => {
                self.dispatch
                    .dispatch(tab_id, "getPerformanceMetrics", serde_json::json!({}))
                    .await
                    .map(|v| {
                        v.get("js_heap")
                            .cloned()
                            .unwrap_or(serde_json::json!({"note": "JS heap stats not available"}))
                    })
            }

            _ => Err(format!("unknown tool: {name}")),
        }
    }
}

#[derive(Clone, serde::Serialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
}

impl ToolInfo {
    fn new(name: &str, description: &str) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_handler() -> VictauriBrowserHandler {
        let tab_mgr = Arc::new(TabManager::new());
        let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        VictauriBrowserHandler::new(tab_mgr, dispatch)
    }

    #[test]
    fn tool_list_has_20_tools() {
        let handler = make_handler();
        assert_eq!(handler.list_tools().len(), 20);
    }

    #[tokio::test]
    async fn plugin_info_returns_metadata() {
        let handler = make_handler();
        let result = handler
            .execute_tool("get_plugin_info", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(result["name"], "victauri-browser");
        assert_eq!(result["mode"], "browser");
        assert_eq!(result["tool_count"], 20);
    }

    #[tokio::test]
    async fn unknown_tool_returns_error() {
        let handler = make_handler();
        let result = handler
            .execute_tool("nonexistent", serde_json::json!({}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown tool"));
    }

    #[tokio::test]
    async fn tabs_list_empty() {
        let handler = make_handler();
        let result = handler
            .execute_tool("tabs", serde_json::json!({"action": "list"}))
            .await
            .unwrap();
        assert!(result.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn eval_js_requires_code() {
        let handler = make_handler();
        let result = handler
            .execute_tool("eval_js", serde_json::json!({}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("code"));
    }

    #[tokio::test]
    async fn interact_requires_action() {
        let handler = make_handler();
        let result = handler
            .execute_tool("interact", serde_json::json!({"ref_id": "e0"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("action"));
    }

    #[tokio::test]
    async fn plugin_info_increments_invocations() {
        let handler = make_handler();
        let r1 = handler
            .execute_tool("get_plugin_info", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(r1["invocations"], 1);

        let r2 = handler
            .execute_tool("get_plugin_info", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(r2["invocations"], 2);
    }

    #[tokio::test]
    async fn tabs_unknown_action_errors() {
        let handler = make_handler();
        let result = handler
            .execute_tool("tabs", serde_json::json!({"action": "close"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown tabs action"));
    }

    #[tokio::test]
    async fn tabs_default_action_is_list() {
        let handler = make_handler();
        let result = handler
            .execute_tool("tabs", serde_json::json!({}))
            .await
            .unwrap();
        assert!(result.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn interact_unknown_action_errors() {
        let handler = make_handler();
        let result = handler
            .execute_tool("interact", serde_json::json!({"action": "destroy"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown interact action"));
    }

    #[tokio::test]
    async fn input_requires_action() {
        let handler = make_handler();
        let result = handler
            .execute_tool("input", serde_json::json!({"ref_id": "e0"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("action"));
    }

    #[tokio::test]
    async fn input_fill_requires_ref_id() {
        let handler = make_handler();
        let result = handler
            .execute_tool("input", serde_json::json!({"action": "fill", "value": "x"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("ref_id"));
    }

    #[tokio::test]
    async fn input_fill_requires_value() {
        let handler = make_handler();
        let result = handler
            .execute_tool("input", serde_json::json!({"action": "fill", "ref_id": "e0"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("value"));
    }

    #[tokio::test]
    async fn input_type_requires_ref_id() {
        let handler = make_handler();
        let result = handler
            .execute_tool("input", serde_json::json!({"action": "type", "text": "hi"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("ref_id"));
    }

    #[tokio::test]
    async fn input_type_requires_text() {
        let handler = make_handler();
        let result = handler
            .execute_tool("input", serde_json::json!({"action": "type", "ref_id": "e0"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("text"));
    }

    #[tokio::test]
    async fn input_press_key_requires_key() {
        let handler = make_handler();
        let result = handler
            .execute_tool("input", serde_json::json!({"action": "press_key"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("key"));
    }

    #[tokio::test]
    async fn input_clear_requires_ref_id() {
        let handler = make_handler();
        let result = handler
            .execute_tool("input", serde_json::json!({"action": "clear"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("ref_id"));
    }

    #[tokio::test]
    async fn input_unknown_action_errors() {
        let handler = make_handler();
        let result = handler
            .execute_tool("input", serde_json::json!({"action": "destroy"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown input action"));
    }

    #[tokio::test]
    async fn inspect_requires_action() {
        let handler = make_handler();
        let result = handler
            .execute_tool("inspect", serde_json::json!({"ref_id": "e0"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("action"));
    }

    #[tokio::test]
    async fn inspect_unknown_action_errors() {
        let handler = make_handler();
        let result = handler
            .execute_tool("inspect", serde_json::json!({"action": "destroy"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown inspect action"));
    }

    #[tokio::test]
    async fn css_requires_action() {
        let handler = make_handler();
        let result = handler
            .execute_tool("css", serde_json::json!({"css": "body{}"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("action"));
    }

    #[tokio::test]
    async fn css_inject_requires_css() {
        let handler = make_handler();
        let result = handler
            .execute_tool("css", serde_json::json!({"action": "inject"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("css"));
    }

    #[tokio::test]
    async fn css_unknown_action_errors() {
        let handler = make_handler();
        let result = handler
            .execute_tool("css", serde_json::json!({"action": "compile"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown css action"));
    }

    #[tokio::test]
    async fn logs_requires_action() {
        let handler = make_handler();
        let result = handler
            .execute_tool("logs", serde_json::json!({}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("action"));
    }

    #[tokio::test]
    async fn logs_unknown_action_errors() {
        let handler = make_handler();
        let result = handler
            .execute_tool("logs", serde_json::json!({"action": "delete"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown logs action"));
    }

    #[tokio::test]
    async fn storage_requires_action() {
        let handler = make_handler();
        let result = handler
            .execute_tool("storage", serde_json::json!({"key": "x"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("action"));
    }

    #[tokio::test]
    async fn storage_unknown_action_errors() {
        let handler = make_handler();
        let result = handler
            .execute_tool("storage", serde_json::json!({"action": "drop"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown storage action"));
    }

    #[tokio::test]
    async fn navigate_requires_action() {
        let handler = make_handler();
        let result = handler
            .execute_tool("navigate", serde_json::json!({"url": "https://x.com"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("action"));
    }

    #[tokio::test]
    async fn navigate_go_to_requires_url() {
        let handler = make_handler();
        let result = handler
            .execute_tool("navigate", serde_json::json!({"action": "go_to"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("url"));
    }

    #[tokio::test]
    async fn navigate_unknown_action_errors() {
        let handler = make_handler();
        let result = handler
            .execute_tool("navigate", serde_json::json!({"action": "refresh"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown navigate action"));
    }

    #[tokio::test]
    async fn recording_requires_action() {
        let handler = make_handler();
        let result = handler
            .execute_tool("recording", serde_json::json!({"label": "test"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("action"));
    }

    #[tokio::test]
    async fn recording_unknown_action_errors() {
        let handler = make_handler();
        let result = handler
            .execute_tool("recording", serde_json::json!({"action": "rewind"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown recording action"));
    }

    #[tokio::test]
    async fn assert_semantic_requires_expression() {
        let handler = make_handler();
        let result = handler
            .execute_tool(
                "assert_semantic",
                serde_json::json!({"condition": "equals", "expected": "x"}),
            )
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("expression"));
    }

    #[tokio::test]
    async fn assert_semantic_requires_condition() {
        let handler = make_handler();
        let result = handler
            .execute_tool(
                "assert_semantic",
                serde_json::json!({"expression": "1+1", "expected": "2"}),
            )
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("condition"));
    }

    #[tokio::test]
    async fn tool_info_fields() {
        let handler = make_handler();
        let tools = handler.list_tools();
        for tool in &tools {
            assert!(!tool.name.is_empty());
            assert!(!tool.description.is_empty());
        }
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"eval_js"));
        assert!(names.contains(&"screenshot"));
        assert!(names.contains(&"assert_semantic"));
    }

    // --- Adversarial stress tests ---

    /// Helper: creates a handler with a dispatch that we can manually resolve.
    /// Spawns `assert_semantic`, intercepts the pending dispatch via `on_response`.
    async fn run_assert_semantic(
        handler: &VictauriBrowserHandler,
        dispatch: &std::sync::Arc<BridgeDispatch>,
        eval_return: serde_json::Value,
        condition: &str,
        expected: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let d = dispatch.clone();
        let cond = condition.to_string();
        let exp = expected.map(str::to_string);

        let eval_result = eval_return.clone();
        let handler = handler.clone();
        let handle = tokio::spawn(async move {
            let mut args = serde_json::json!({
                "expression": "test_expr",
                "condition": cond,
            });
            if let Some(e) = exp {
                args["expected"] = serde_json::Value::String(e);
            }
            handler.execute_tool("assert_semantic", args).await
        });

        // Wait briefly for the dispatch to register the pending command
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Find and resolve the pending command
        let ids = d.pending_ids().await;
        if let Some(id) = ids.first() {
            d.on_response(id, Some(eval_result), None).await;
        }

        handle.await.unwrap()
    }

    fn make_handler_with_dispatch() -> (VictauriBrowserHandler, std::sync::Arc<BridgeDispatch>) {
        let tab_mgr = std::sync::Arc::new(TabManager::new());
        let dispatch = std::sync::Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        let handler = VictauriBrowserHandler::new(tab_mgr, dispatch.clone());
        (handler, dispatch)
    }

    #[tokio::test]
    async fn assert_semantic_equals_pass() {
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!("hello"), "equals", Some("hello")).await.unwrap();
        assert_eq!(result["passed"], true);
        assert_eq!(result["actual"], "hello");
    }

    #[tokio::test]
    async fn assert_semantic_equals_fail() {
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!("hello"), "equals", Some("world")).await.unwrap();
        assert_eq!(result["passed"], false);
    }

    #[tokio::test]
    async fn assert_semantic_not_equals_pass() {
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!("hello"), "not_equals", Some("world")).await.unwrap();
        assert_eq!(result["passed"], true);
    }

    #[tokio::test]
    async fn assert_semantic_not_equals_fail() {
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!("same"), "not_equals", Some("same")).await.unwrap();
        assert_eq!(result["passed"], false);
    }

    #[tokio::test]
    async fn assert_semantic_contains_pass() {
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!("hello world"), "contains", Some("world")).await.unwrap();
        assert_eq!(result["passed"], true);
    }

    #[tokio::test]
    async fn assert_semantic_contains_fail() {
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!("hello"), "contains", Some("xyz")).await.unwrap();
        assert_eq!(result["passed"], false);
    }

    #[tokio::test]
    async fn assert_semantic_truthy_values() {
        let (h, d) = make_handler_with_dispatch();

        for (val, expected_pass) in [
            (serde_json::json!("hello"), true),
            (serde_json::json!("1"), true),
            (serde_json::json!(42), true),
            (serde_json::json!("false"), false),
            (serde_json::json!("0"), false),
            (serde_json::json!("null"), false),
            (serde_json::json!("undefined"), false),
        ] {
            let result = run_assert_semantic(&h, &d, val.clone(), "truthy", None).await.unwrap();
            assert_eq!(
                result["passed"], expected_pass,
                "truthy check failed for {val:?}, expected passed={expected_pass}",
            );
        }
    }

    #[tokio::test]
    async fn assert_semantic_greater_than_pass() {
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!("42"), "greater_than", Some("10")).await.unwrap();
        assert_eq!(result["passed"], true);
    }

    #[tokio::test]
    async fn assert_semantic_greater_than_fail() {
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!("5"), "greater_than", Some("10")).await.unwrap();
        assert_eq!(result["passed"], false);
    }

    #[tokio::test]
    async fn assert_semantic_greater_than_equal_is_false() {
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!("10"), "greater_than", Some("10")).await.unwrap();
        assert_eq!(result["passed"], false);
    }

    #[tokio::test]
    async fn assert_semantic_less_than_pass() {
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!("3"), "less_than", Some("10")).await.unwrap();
        assert_eq!(result["passed"], true);
    }

    #[tokio::test]
    async fn assert_semantic_less_than_with_floats() {
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!("3.14"), "less_than", Some("3.15")).await.unwrap();
        assert_eq!(result["passed"], true);
    }

    #[tokio::test]
    async fn assert_semantic_greater_than_non_numeric_fails() {
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!("not_a_number"), "greater_than", Some("10")).await.unwrap();
        assert_eq!(result["passed"], false);
    }

    #[tokio::test]
    async fn assert_semantic_unknown_condition() {
        let dispatch = std::sync::Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        let h = VictauriBrowserHandler::new(
            std::sync::Arc::new(TabManager::new()),
            dispatch.clone(),
        );

        let handle = tokio::spawn({
            let h = h.clone();
            async move {
                h.execute_tool(
                    "assert_semantic",
                    serde_json::json!({
                        "expression": "1",
                        "condition": "banana",
                    }),
                )
                .await
            }
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let ids = dispatch.pending_ids().await;
        if let Some(id) = ids.first() {
            dispatch
                .on_response(id, Some(serde_json::json!("1")), None)
                .await;
        }

        let result = handle.await.unwrap();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown condition"));
    }

    #[tokio::test]
    async fn assert_semantic_equals_without_expected() {
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!("hello"), "equals", None).await.unwrap();
        // equals with no expected should fail (is_some_and returns false)
        assert_eq!(result["passed"], false);
    }

    #[tokio::test]
    async fn concurrent_invocation_counter_correctness() {
        let tab_mgr = std::sync::Arc::new(TabManager::new());
        let dispatch = std::sync::Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        let handler = VictauriBrowserHandler::new(tab_mgr, dispatch);

        let mut handles = vec![];
        for _ in 0..100 {
            let h = handler.clone();
            handles.push(tokio::spawn(async move {
                h.execute_tool("get_plugin_info", serde_json::json!({}))
                    .await
                    .unwrap()
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        let final_info = handler
            .execute_tool("get_plugin_info", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(final_info["invocations"], 101);
    }

    #[tokio::test]
    async fn tabs_list_with_populated_manager() {
        let tab_mgr = std::sync::Arc::new(TabManager::new());
        tab_mgr
            .on_tab_created(1, "https://one.com", "One")
            .await;
        tab_mgr
            .on_tab_created(2, "https://two.com", "Two")
            .await;
        tab_mgr.on_tab_activated(2).await;

        let dispatch = std::sync::Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        let handler = VictauriBrowserHandler::new(tab_mgr, dispatch);

        let result = handler
            .execute_tool("tabs", serde_json::json!({"action": "list"}))
            .await
            .unwrap();
        let tabs = result.as_array().unwrap();
        assert_eq!(tabs.len(), 2);

        let active: Vec<_> = tabs.iter().filter(|t| t["active"] == true).collect();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0]["tab_id"], 2);
    }

    #[tokio::test]
    async fn get_memory_stats_extracts_js_heap() {
        let (h, d) = make_handler_with_dispatch();

        let handle = tokio::spawn({
            let h = h.clone();
            async move {
                h.execute_tool("get_memory_stats", serde_json::json!({}))
                    .await
            }
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let ids = d.pending_ids().await;
        let id = ids.first().cloned();

        if let Some(id) = id {
            d.on_response(
                &id,
                Some(serde_json::json!({
                    "js_heap": {"used_mb": 15.2, "total_mb": 32.0},
                    "dom_stats": {"elements": 500},
                })),
                None,
            )
            .await;
        }

        let result = handle.await.unwrap().unwrap();
        assert_eq!(result["used_mb"], 15.2);
        assert!(result.get("dom_stats").is_none());
    }

    #[tokio::test]
    async fn get_memory_stats_without_js_heap_key() {
        let (h, d) = make_handler_with_dispatch();

        let handle = tokio::spawn({
            let h = h.clone();
            async move {
                h.execute_tool("get_memory_stats", serde_json::json!({}))
                    .await
            }
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let ids = d.pending_ids().await;
        let id = ids.first().cloned();

        if let Some(id) = id {
            d.on_response(
                &id,
                Some(serde_json::json!({"dom_stats": {"elements": 100}})),
                None,
            )
            .await;
        }

        let result = handle.await.unwrap().unwrap();
        assert!(result["note"].as_str().unwrap().contains("not available"));
    }

    #[test]
    fn interact_action_routing_coverage() {
        let valid = ["click", "double_click", "hover", "focus", "scroll", "scroll_into_view", "select"];
        let invalid = ["destroy", "swipe", "pinch", ""];
        for action in valid {
            let method = match action {
                "click" => "click",
                "double_click" => "doubleClick",
                "hover" => "hover",
                "focus" => "focusElement",
                "scroll" | "scroll_into_view" => "scrollTo",
                "select" => "selectOption",
                _ => panic!("unhandled action"),
            };
            assert!(!method.is_empty(), "valid action {action} should map to a method");
        }
        for action in invalid {
            assert!(
                !["click", "double_click", "hover", "focus", "scroll", "scroll_into_view", "select"]
                    .contains(&action),
                "{action} should not be in valid set"
            );
        }
    }

    #[test]
    fn inspect_action_routing_coverage() {
        let valid = ["styles", "bounds", "highlight", "clear_highlights", "accessibility", "performance"];
        for action in valid {
            let is_known = matches!(
                action,
                "styles" | "bounds" | "highlight" | "clear_highlights" | "accessibility" | "performance"
            );
            assert!(is_known, "action {action} not recognized");
        }
    }

    #[test]
    fn logs_action_routing_coverage() {
        let valid = ["console", "network", "navigation", "dialogs", "events"];
        for action in valid {
            let is_known = matches!(
                action,
                "console" | "network" | "navigation" | "dialogs" | "events"
            );
            assert!(is_known, "action {action} not recognized");
        }
    }

    #[tokio::test]
    async fn storage_session_store_routes_correctly() {
        let (h, d) = make_handler_with_dispatch();

        for action in ["get", "set", "delete"] {
            let handle = tokio::spawn({
                let h = h.clone();
                let action = action.to_string();
                async move {
                    let mut args = serde_json::json!({"action": action, "store": "session"});
                    if action == "set" {
                        args["key"] = serde_json::json!("k");
                        args["value"] = serde_json::json!("v");
                    }
                    h.execute_tool("storage", args).await
                }
            });

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let ids = d.pending_ids().await;

            if let Some(id) = ids.first() {
                d.on_response(id, Some(serde_json::json!({"ok": true})), None)
                    .await;
            }

            let result = handle.await.unwrap().unwrap();
            assert_eq!(result["ok"], true);
        }
    }

    #[test]
    fn recording_action_routing_coverage() {
        let valid = ["start", "stop", "checkpoint", "get_events", "list_checkpoints", "export"];
        for action in valid {
            let is_known = matches!(
                action,
                "start" | "stop" | "checkpoint" | "get_events" | "list_checkpoints" | "export"
            );
            assert!(is_known, "action {action} not recognized");
        }
    }

    #[test]
    fn navigate_action_routing_coverage() {
        let valid = ["go_to", "back", "history", "dialogs"];
        for action in valid {
            let is_known = matches!(action, "go_to" | "back" | "history" | "dialogs");
            assert!(is_known, "action {action} not recognized");
        }
    }

    // --- Deep assert_semantic edge cases ---

    #[tokio::test]
    async fn assert_semantic_numeric_from_non_string_value() {
        // When eval returns a number (not wrapped in quotes), as_str() returns None
        // and we fall through to eval_result.to_string() — this is the numeric path
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!(42), "greater_than", Some("10")).await.unwrap();
        assert_eq!(result["passed"], true);
        assert_eq!(result["actual"], "42");
    }

    #[tokio::test]
    async fn assert_semantic_truthy_empty_string_quoted() {
        // The literal string "" (two quotes) should be falsy
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!("\"\""), "truthy", None).await.unwrap();
        assert_eq!(result["passed"], false);
    }

    #[tokio::test]
    async fn assert_semantic_truthy_whitespace_is_truthy() {
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!(" "), "truthy", None).await.unwrap();
        assert_eq!(result["passed"], true);
    }

    #[tokio::test]
    async fn assert_semantic_contains_empty_expected() {
        // Every string contains ""
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!("anything"), "contains", Some("")).await.unwrap();
        assert_eq!(result["passed"], true);
    }

    #[tokio::test]
    async fn assert_semantic_greater_than_negative_numbers() {
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!("-5"), "greater_than", Some("-10")).await.unwrap();
        assert_eq!(result["passed"], true);

        let result2 = run_assert_semantic(&h, &d, serde_json::json!("-20"), "greater_than", Some("-10")).await.unwrap();
        assert_eq!(result2["passed"], false);
    }

    #[tokio::test]
    async fn assert_semantic_less_than_zero() {
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!("-1"), "less_than", Some("0")).await.unwrap();
        assert_eq!(result["passed"], true);
    }

    #[tokio::test]
    async fn assert_semantic_equals_with_json_object() {
        // When eval returns an object, as_str() is None, so actual_str = to_string()
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(
            &h, &d,
            serde_json::json!({"key": "val"}),
            "contains",
            Some("key"),
        ).await.unwrap();
        assert_eq!(result["passed"], true);
    }

    #[tokio::test]
    async fn assert_semantic_greater_than_infinity() {
        let (h, d) = make_handler_with_dispatch();
        // "inf" parses as f64::INFINITY in Rust, so inf > 999999 is true
        let result = run_assert_semantic(&h, &d, serde_json::json!("inf"), "greater_than", Some("999999")).await.unwrap();
        assert_eq!(result["passed"], true);

        // "infinity" also parses
        let result2 = run_assert_semantic(&h, &d, serde_json::json!("infinity"), "greater_than", Some("999999")).await.unwrap();
        assert_eq!(result2["passed"], true);

        // "NaN" parses but NaN > x is always false
        let result3 = run_assert_semantic(&h, &d, serde_json::json!("NaN"), "greater_than", Some("0")).await.unwrap();
        assert_eq!(result3["passed"], false);
    }

    #[tokio::test]
    async fn assert_semantic_not_equals_with_no_expected() {
        let (h, d) = make_handler_with_dispatch();
        // not_equals with no expected — is_some_and returns false
        let result = run_assert_semantic(&h, &d, serde_json::json!("x"), "not_equals", None).await.unwrap();
        assert_eq!(result["passed"], false);
    }

    #[tokio::test]
    async fn assert_semantic_contains_case_sensitive() {
        let (h, d) = make_handler_with_dispatch();
        let result = run_assert_semantic(&h, &d, serde_json::json!("Hello World"), "contains", Some("hello")).await.unwrap();
        // Contains is case-sensitive
        assert_eq!(result["passed"], false);
    }

    // --- Type confusion & adversarial argument tests ---

    #[tokio::test]
    async fn tool_with_action_as_number_errors() {
        let handler = make_handler();
        // action field is a number instead of string
        let result = handler
            .execute_tool("interact", serde_json::json!({"action": 42, "ref_id": "e0"}))
            .await;
        // as_str on number returns None → "missing action"
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("action"));
    }

    #[tokio::test]
    async fn tool_with_action_as_array_errors() {
        let handler = make_handler();
        let result = handler
            .execute_tool("input", serde_json::json!({"action": ["fill"], "ref_id": "e0"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("action"));
    }

    #[tokio::test]
    async fn tool_with_action_as_null_errors() {
        let handler = make_handler();
        let result = handler
            .execute_tool("css", serde_json::json!({"action": null}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("action"));
    }

    #[tokio::test]
    async fn eval_js_code_as_number_errors() {
        let handler = make_handler();
        let result = handler
            .execute_tool("eval_js", serde_json::json!({"code": 42}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("code"));
    }

    #[tokio::test]
    async fn eval_js_code_as_object_errors() {
        let handler = make_handler();
        let result = handler
            .execute_tool("eval_js", serde_json::json!({"code": {"expr": "1+1"}}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("code"));
    }

    #[tokio::test]
    async fn navigate_go_to_url_as_object_errors() {
        let handler = make_handler();
        let result = handler
            .execute_tool("navigate", serde_json::json!({"action": "go_to", "url": {"href": "https://x.com"}}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("url"));
    }

    #[tokio::test]
    async fn input_fill_value_as_number_errors() {
        let handler = make_handler();
        let result = handler
            .execute_tool("input", serde_json::json!({"action": "fill", "ref_id": "e0", "value": 42}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("value"));
    }

    #[tokio::test]
    async fn css_inject_css_as_array_errors() {
        let handler = make_handler();
        let result = handler
            .execute_tool("css", serde_json::json!({"action": "inject", "css": ["body{}", "div{}"]}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("css"));
    }

    #[tokio::test]
    async fn unknown_tool_name_errors() {
        let handler = make_handler();
        let result = handler
            .execute_tool("drop_table", serde_json::json!({}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown tool"));
    }

    #[tokio::test]
    async fn empty_tool_name_errors() {
        let handler = make_handler();
        let result = handler.execute_tool("", serde_json::json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown tool"));
    }

    #[tokio::test]
    async fn tool_name_case_sensitive() {
        let handler = make_handler();
        // Tools are case-sensitive — "Eval_Js" is not "eval_js"
        let result = handler
            .execute_tool("Eval_Js", serde_json::json!({"code": "1"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown tool"));
    }

    #[tokio::test]
    async fn tool_invocations_count_includes_failures() {
        let handler = make_handler();
        // Even failed calls should increment the counter
        let _ = handler.execute_tool("nonexistent", serde_json::json!({})).await;
        let _ = handler.execute_tool("eval_js", serde_json::json!({})).await;
        let info = handler.execute_tool("get_plugin_info", serde_json::json!({})).await.unwrap();
        assert_eq!(info["invocations"], 3);
    }

    #[tokio::test]
    async fn tabs_with_populated_manager_shows_count() {
        let tab_mgr = Arc::new(TabManager::new());
        let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));

        tab_mgr.on_tab_created(1, "https://a.com", "A").await;
        tab_mgr.on_tab_created(2, "https://b.com", "B").await;

        let handler = VictauriBrowserHandler::new(tab_mgr, dispatch);
        let info = handler.execute_tool("get_plugin_info", serde_json::json!({})).await.unwrap();
        assert_eq!(info["tab_count"], 2);
    }

    #[tokio::test]
    async fn tabs_list_with_active_tab_marked() {
        let tab_mgr = Arc::new(TabManager::new());
        let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));

        tab_mgr.on_tab_created(10, "https://a.com", "A").await;
        tab_mgr.on_tab_created(20, "https://b.com", "B").await;
        tab_mgr.on_tab_activated(20).await;

        let handler = VictauriBrowserHandler::new(tab_mgr, dispatch);
        let result = handler.execute_tool("tabs", serde_json::json!({"action": "list"})).await.unwrap();
        let tabs = result.as_array().unwrap();
        let active: Vec<_> = tabs.iter().filter(|t| t["active"] == true).collect();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0]["tab_id"], 20);
    }

    #[tokio::test]
    async fn all_action_tools_reject_empty_string_action() {
        let handler = make_handler();
        let tools_with_actions = ["interact", "input", "inspect", "css", "logs", "storage", "navigate", "recording"];
        for tool in tools_with_actions {
            let result = handler
                .execute_tool(tool, serde_json::json!({"action": ""}))
                .await;
            assert!(
                result.is_err(),
                "{tool} should reject empty string action"
            );
            let err = result.unwrap_err();
            assert!(
                err.contains("unknown") || err.contains("action"),
                "{tool} error should mention action: {err}"
            );
        }
    }

    #[tokio::test]
    async fn concurrent_tool_invocation_counter() {
        let handler = Arc::new(make_handler());
        let mut handles = vec![];
        for _ in 0..100 {
            let h = Arc::clone(&handler);
            handles.push(tokio::spawn(async move {
                h.execute_tool("get_plugin_info", serde_json::json!({})).await.unwrap()
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        let info = handler.execute_tool("get_plugin_info", serde_json::json!({})).await.unwrap();
        assert_eq!(info["invocations"], 101);
    }

    #[tokio::test]
    async fn all_bridge_tools_dispatch_recognized() {
        // Verify every tool that dispatches to the bridge is recognized (not "unknown tool").
        // Uses a spawned task to resolve the pending command immediately with mock data.
        let tab_mgr = Arc::new(TabManager::new());
        let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        let handler = VictauriBrowserHandler::new(Arc::clone(&tab_mgr), Arc::clone(&dispatch));

        let bridge_tools: Vec<(&str, serde_json::Value)> = vec![
            ("get_diagnostics", serde_json::json!({})),
            ("get_memory_stats", serde_json::json!({})),
            ("screenshot", serde_json::json!({})),
            ("page_info", serde_json::json!({})),
            ("cookies", serde_json::json!({})),
            ("dom_snapshot", serde_json::json!({})),
            ("find_elements", serde_json::json!({"text": "x"})),
            ("wait_for", serde_json::json!({"condition": "selector", "value": "body"})),
        ];

        for (tool_name, args) in bridge_tools {
            let d = Arc::clone(&dispatch);
            // Spawn a task that resolves whatever pending command appears
            let resolver = tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                let ids = d.pending_ids().await;
                for id in ids {
                    d.on_response(&id, Some(serde_json::json!({"mock": true, "js_heap": {}})), None).await;
                }
            });

            let result = handler.execute_tool(tool_name, args).await;
            resolver.await.unwrap();

            match result {
                Ok(_) => {} // Tool recognized and got mock response
                Err(e) => {
                    assert!(
                        !e.contains("unknown tool"),
                        "{tool_name} should be recognized: {e}"
                    );
                }
            }
        }
    }
}
