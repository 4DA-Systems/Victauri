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
}
