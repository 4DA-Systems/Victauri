mod backend_params;
mod compound_params;
mod helpers;
mod other_params;
mod rest;
mod server;
mod verification_params;
mod webview_params;
mod window_params;

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    AnnotateAble, CallToolRequestParams, CallToolResult, Content, ListResourcesResult,
    ListToolsResult, PaginatedRequestParams, RawContent, RawResource, ReadResourceRequestParams,
    ReadResourceResult, ResourceContents, ServerCapabilities, ServerInfo, SubscribeRequestParams,
    Tool, UnsubscribeRequestParams,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, tool, tool_router};
use tokio::sync::Mutex;

use crate::VictauriState;
use crate::bridge::WebviewBridge;

use helpers::{
    js_string, json_result, missing_param, sanitize_css_color, tool_disabled, tool_error,
    validate_url,
};

pub use backend_params::*;
pub use compound_params::*;
pub use other_params::{
    DiagnosticsParams, FindElementsParams, ResolveCommandParams, SemanticAssertParams,
    WaitCondition, WaitForParams,
};
pub use server::*;
pub use verification_params::*;
pub use webview_params::*;
pub use window_params::*;

// ── MCP Handler ──────────────────────────────────────────────────────────────

/// Maximum number of in-flight JavaScript eval requests. Prevents unbounded
/// growth of the `pending_evals` map if callbacks are never resolved.
pub(crate) const MAX_PENDING_EVALS: usize = 100;

/// Maximum length of JavaScript code accepted by the `eval_js` tool (1 MB).
const MAX_EVAL_CODE_LEN: usize = 1_000_000;

const RESOURCE_URI_IPC_LOG: &str = "victauri://ipc-log";
const RESOURCE_URI_WINDOWS: &str = "victauri://windows";
const RESOURCE_URI_STATE: &str = "victauri://state";

const BRIDGE_VERSION: &str = "0.3.0";

const SAFE_ENV_PREFIXES: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LANG",
    "LC_",
    "TERM",
    "SHELL",
    "DISPLAY",
    "XDG_",
    "TAURI_",
    "VICTAURI_",
    "RUST",
    "CARGO",
    "NODE_ENV",
    "APPDATA",
    "LOCALAPPDATA",
    "USERPROFILE",
    "TEMP",
    "TMP",
    "PROGRAMFILES",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "OS",
    "PROCESSOR_",
    "NUMBER_OF_PROCESSORS",
    "COMPUTERNAME",
    "HOSTNAME",
    "PWD",
    "OLDPWD",
    "SHLVL",
    "LOGNAME",
];

/// MCP tool handler that dispatches tool calls to the webview bridge and state.
#[derive(Clone)]
pub struct VictauriMcpHandler {
    state: Arc<VictauriState>,
    bridge: Arc<dyn WebviewBridge>,
    subscriptions: Arc<Mutex<HashSet<String>>>,
    bridge_checked: Arc<AtomicBool>,
}

#[tool_router]
impl VictauriMcpHandler {
    // ── Standalone Tools ────────────────────────────────────────────────────

    #[tool(
        description = "Evaluate JavaScript in the Tauri webview and return the result. Async expressions are wrapped automatically.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn eval_js(&self, Parameters(params): Parameters<EvalJsParams>) -> CallToolResult {
        if !self.state.privacy.is_tool_enabled("eval_js") {
            return tool_disabled("eval_js");
        }
        if params.code.len() > MAX_EVAL_CODE_LEN {
            return tool_error("code exceeds maximum length (1 MB)");
        }
        match self
            .eval_with_return(&params.code, params.webview_label.as_deref())
            .await
        {
            Ok(result) => CallToolResult::success(vec![Content::text(result)]),
            Err(e) => tool_error(e),
        }
    }

    #[tool(
        description = "Get the DOM snapshot with stable ref handles. Default: compact accessible text (70-80%% fewer tokens). Set format=\"json\" for full tree. Returns tree + stale_refs (refs invalidated since last snapshot).",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn dom_snapshot(&self, Parameters(params): Parameters<SnapshotParams>) -> CallToolResult {
        let format = params.format.unwrap_or(SnapshotFormat::Compact);
        let format_str = match format {
            SnapshotFormat::Compact => "compact",
            SnapshotFormat::Json => "json",
        };
        let code = format!(
            "return window.__VICTAURI__?.snapshot({})",
            js_string(format_str)
        );
        self.eval_bridge(&code, params.webview_label.as_deref())
            .await
    }

    #[tool(
        description = "Search for elements by text, role, test_id, CSS selector (via `css` or `selector` param), or accessible name without a full snapshot. Returns lightweight matches with ref handles.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn find_elements(
        &self,
        Parameters(params): Parameters<FindElementsParams>,
    ) -> CallToolResult {
        let mut parts: Vec<String> = Vec::new();
        if let Some(t) = &params.text {
            parts.push(format!("text: {}", js_string(t)));
        }
        if let Some(r) = &params.role {
            parts.push(format!("role: {}", js_string(r)));
        }
        if let Some(tid) = &params.test_id {
            parts.push(format!("test_id: {}", js_string(tid)));
        }
        if let Some(c) = params.css.as_ref().or(params.selector.as_ref()) {
            parts.push(format!("css: {}", js_string(c)));
        }
        if let Some(n) = &params.name {
            parts.push(format!("name: {}", js_string(n)));
        }
        if let Some(max) = params.max_results {
            parts.push(format!("max_results: {max}"));
        }
        if let Some(t) = &params.tag {
            parts.push(format!("tag: {}", js_string(t)));
        }
        if let Some(p) = &params.placeholder {
            parts.push(format!("placeholder: {}", js_string(p)));
        }
        if let Some(a) = &params.alt {
            parts.push(format!("alt: {}", js_string(a)));
        }
        if let Some(ta) = &params.title_attr {
            parts.push(format!("title_attr: {}", js_string(ta)));
        }
        if let Some(l) = &params.label {
            parts.push(format!("label: {}", js_string(l)));
        }
        if let Some(true) = params.exact {
            parts.push("exact: true".to_string());
        }
        if let Some(e) = params.enabled {
            parts.push(format!("enabled: {e}"));
        }
        let code = format!(
            "return window.__VICTAURI__?.findElements({{ {} }})",
            parts.join(", ")
        );
        self.eval_bridge(&code, params.webview_label.as_deref())
            .await
    }

    #[tool(
        description = "Invoke a registered Tauri command via IPC, just like the frontend would. Goes through the real IPC pipeline so calls are logged and verifiable. Returns the command's result. Subject to privacy command filtering.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn invoke_command(
        &self,
        Parameters(params): Parameters<InvokeCommandParams>,
    ) -> CallToolResult {
        if !self.state.privacy.is_invoke_allowed(&params.command) {
            return tool_disabled("invoke_command");
        }
        if !self.state.privacy.is_command_allowed(&params.command) {
            return tool_error(format!(
                "command '{}' is blocked by privacy configuration",
                params.command
            ));
        }
        let args_json = params.args.unwrap_or(serde_json::json!({}));
        let args_str = serde_json::to_string(&args_json).unwrap_or_else(|_| "{}".to_string());
        let code = format!(
            "return window.__TAURI_INTERNALS__.invoke({}, {args_str})",
            js_string(&params.command)
        );
        match self
            .eval_with_return(&code, params.webview_label.as_deref())
            .await
        {
            Ok(result) => {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&result)
                    && let Some(err) = parsed.get("__error").and_then(|e| e.as_str())
                {
                    return tool_error(format!(
                        "command '{}' returned error: {err}",
                        params.command
                    ));
                }
                CallToolResult::success(vec![Content::text(result)])
            }
            Err(e) => tool_error(format!("invoke_command failed: {e}")),
        }
    }

    #[tool(
        description = "Capture a screenshot of a Tauri window as a base64-encoded PNG image. Works on Windows (PrintWindow), macOS (CGWindowListCreateImage), and Linux (X11/Wayland).",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn screenshot(&self, Parameters(params): Parameters<ScreenshotParams>) -> CallToolResult {
        self.track_tool_call();
        if !self.state.privacy.is_tool_enabled("screenshot") {
            return tool_disabled("screenshot");
        }
        match self
            .bridge
            .get_native_handle(params.window_label.as_deref())
        {
            Ok(hwnd) => match crate::screenshot::capture_window(hwnd).await {
                Ok(png_bytes) => {
                    use base64::Engine;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
                    CallToolResult::success(vec![Content::image(b64, "image/png")])
                }
                Err(e) => tool_error(format!("screenshot capture failed: {e}")),
            },
            Err(e) => tool_error(format!("cannot get window handle: {e}")),
        }
    }

    #[tool(
        description = "Compare frontend state (evaluated via JS expression) against backend state to detect divergences. Returns a VerificationResult with any mismatches.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn verify_state(
        &self,
        Parameters(params): Parameters<VerifyStateParams>,
    ) -> CallToolResult {
        if !self.state.privacy.is_tool_enabled("eval_js") {
            return tool_disabled("verify_state requires eval_js capability");
        }
        let code = format!("return ({})", params.frontend_expr);
        let frontend_json = match self
            .eval_with_return(&code, params.webview_label.as_deref())
            .await
        {
            Ok(result) => result,
            Err(e) => return tool_error(format!("failed to evaluate frontend expression: {e}")),
        };

        let frontend_state: serde_json::Value = match serde_json::from_str(&frontend_json) {
            Ok(v) => v,
            Err(e) => {
                return tool_error(format!(
                    "frontend expression did not return valid JSON: {e}"
                ));
            }
        };

        let backend_state = if let Some(state) = params.backend_state {
            state
        } else if let Some(ref cmd) = params.backend_command {
            if !self.state.privacy.is_command_allowed(cmd) {
                return tool_error(format!(
                    "command '{cmd}' is blocked by privacy configuration"
                ));
            }
            let args = params.backend_args.unwrap_or(serde_json::json!({}));
            let args_str = serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string());
            let invoke_code = format!(
                "return window.__TAURI_INTERNALS__.invoke({}, {args_str})",
                js_string(cmd)
            );
            match self
                .eval_with_return(&invoke_code, params.webview_label.as_deref())
                .await
            {
                Ok(result) => match serde_json::from_str(&result) {
                    Ok(v) => v,
                    Err(e) => {
                        return tool_error(format!(
                            "backend command '{cmd}' did not return valid JSON: {e}"
                        ));
                    }
                },
                Err(e) => {
                    return tool_error(format!("failed to invoke backend command '{cmd}': {e}"));
                }
            }
        } else {
            return tool_error("either backend_state or backend_command must be provided");
        };

        let result = victauri_core::verify_state(frontend_state, backend_state);
        json_result(&result)
    }

    #[tool(
        description = "Detect ghost commands — commands invoked from the frontend that have no backend handler, or registered backend commands never called. Reads from the JS-side IPC interception log.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn detect_ghost_commands(
        &self,
        Parameters(params): Parameters<GhostCommandParams>,
    ) -> CallToolResult {
        let code = "return window.__VICTAURI__?.getIpcLog()";
        let ipc_json = match self
            .eval_with_return(code, params.webview_label.as_deref())
            .await
        {
            Ok(r) => r,
            Err(e) => return tool_error(format!("failed to read IPC log: {e}")),
        };

        let ipc_calls: Vec<serde_json::Value> = match serde_json::from_str(&ipc_json) {
            Ok(v) => v,
            Err(e) => return tool_error(format!("failed to parse IPC log JSON: {e}")),
        };
        let frontend_commands: Vec<String> = ipc_calls
            .iter()
            .filter_map(|c| c.get("command").and_then(|v| v.as_str()).map(String::from))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let report = victauri_core::detect_ghost_commands(&frontend_commands, &self.state.registry);
        json_result(&report)
    }

    #[tool(
        description = "Check IPC round-trip integrity: find stale (stuck) pending calls and errored calls. Returns health status and lists of problematic IPC calls.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn check_ipc_integrity(
        &self,
        Parameters(params): Parameters<IpcIntegrityParams>,
    ) -> CallToolResult {
        let threshold = params.stale_threshold_ms.unwrap_or(5000);
        let code = format!(
            r"return (function() {{
                var log = window.__VICTAURI__?.getIpcLog() || [];
                var now = Date.now();
                var threshold = {threshold};
                var pending = log.filter(function(c) {{ return c.status === 'pending'; }});
                var stale = pending.filter(function(c) {{ return (now - c.timestamp) > threshold; }});
                var errored = log.filter(function(c) {{ return c.status === 'error'; }});
                return {{
                    healthy: stale.length === 0 && errored.length === 0,
                    total_calls: log.length,
                    pending_count: pending.length,
                    stale_count: stale.length,
                    error_count: errored.length,
                    stale_calls: stale.slice(0, 20),
                    errored_calls: errored.slice(0, 20)
                }};
            }})()"
        );
        self.eval_bridge(&code, params.webview_label.as_deref())
            .await
    }

    #[tool(
        description = "Wait for a condition to be met. Polls at regular intervals until satisfied or timeout. Conditions: text (text appears), text_gone (text disappears), selector (CSS selector matches), selector_gone, url (URL contains value), ipc_idle (no pending IPC calls), network_idle (no pending network requests).",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn wait_for(&self, Parameters(params): Parameters<WaitForParams>) -> CallToolResult {
        let value = params
            .value
            .as_ref()
            .map_or_else(|| "null".to_string(), |v| js_string(v));
        let timeout_ms = params.timeout_ms.unwrap_or(10_000).min(60_000);
        let poll = params.poll_ms.unwrap_or(200);
        let code = format!(
            "return window.__VICTAURI__?.waitFor({{ condition: {}, value: {value}, timeout_ms: {timeout_ms}, poll_ms: {poll} }})",
            js_string(params.condition.as_str())
        );
        let eval_timeout = std::time::Duration::from_millis(timeout_ms + 5000);
        match self
            .eval_with_return_timeout(&code, params.webview_label.as_deref(), eval_timeout)
            .await
        {
            Ok(result) => CallToolResult::success(vec![Content::text(result)]),
            Err(e) => tool_error(e),
        }
    }

    #[tool(
        description = "Run a semantic assertion: evaluate a JS expression and check the result against an expected condition. Conditions: equals, not_equals, contains, greater_than, less_than, truthy, falsy, exists, type_is.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn assert_semantic(
        &self,
        Parameters(params): Parameters<SemanticAssertParams>,
    ) -> CallToolResult {
        if !self.state.privacy.is_tool_enabled("eval_js") {
            return tool_disabled("assert_semantic requires eval_js capability");
        }
        let code = format!("return ({})", params.expression);
        let actual_json = match self
            .eval_with_return(&code, params.webview_label.as_deref())
            .await
        {
            Ok(result) => result,
            Err(e) => return tool_error(format!("failed to evaluate expression: {e}")),
        };

        let actual: serde_json::Value = match serde_json::from_str(&actual_json) {
            Ok(v) => v,
            Err(e) => return tool_error(format!("expression did not return valid JSON: {e}")),
        };

        let assertion = victauri_core::SemanticAssertion {
            label: params.label,
            condition: params.condition,
            expected: params.expected,
        };

        let result = victauri_core::evaluate_assertion(actual, &assertion);
        json_result(&result)
    }

    #[tool(
        description = "Resolve a natural language query to matching Tauri commands. Returns scored results ranked by relevance, using command names, descriptions, intents, categories, and examples.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn resolve_command(
        &self,
        Parameters(params): Parameters<ResolveCommandParams>,
    ) -> CallToolResult {
        self.track_tool_call();
        let limit = params.limit.unwrap_or(5);
        let mut results = self.state.registry.resolve(&params.query);
        results.truncate(limit);
        json_result(&results)
    }

    #[tool(
        description = "List or search all registered Tauri commands with their argument schemas. Pass query to filter by name/description substring. Commands are registered via #[inspectable] macro.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_registry(&self, Parameters(params): Parameters<RegistryParams>) -> CallToolResult {
        self.track_tool_call();
        let commands = match params.query {
            Some(q) => self.state.registry.search(&q),
            None => self.state.registry.list(),
        };
        json_result(&commands)
    }

    #[tool(
        description = "Get real-time process memory statistics from the OS (working set, page file usage). On Windows returns detailed metrics; on Linux returns virtual/resident size.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_memory_stats(&self) -> CallToolResult {
        self.track_tool_call();
        let stats = crate::memory::current_stats();
        json_result(&stats)
    }

    #[tool(
        description = "Inspect the Victauri plugin's own configuration: port, enabled/disabled tools, command filters, privacy settings, capacities, and version. Useful for agents to understand their capabilities before acting.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_plugin_info(&self) -> CallToolResult {
        self.track_tool_call();
        let disabled: Vec<&str> = self
            .state
            .privacy
            .disabled_tools
            .iter()
            .map(std::string::String::as_str)
            .collect();
        let blocklist: Vec<&str> = self
            .state
            .privacy
            .command_blocklist
            .iter()
            .map(std::string::String::as_str)
            .collect();
        let allowlist: Option<Vec<&str>> = self
            .state
            .privacy
            .command_allowlist
            .as_ref()
            .map(|s| s.iter().map(std::string::String::as_str).collect());
        let all_tools = Self::tool_router().list_all();
        let enabled_tools: Vec<&str> = all_tools
            .iter()
            .filter(|t| self.state.privacy.is_tool_enabled(t.name.as_ref()))
            .map(|t| t.name.as_ref())
            .collect();

        let result = serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "bridge_version": BRIDGE_VERSION,
            "port": self.state.port.load(Ordering::Relaxed),
            "tools": {
                "total": all_tools.len(),
                "enabled": enabled_tools.len(),
                "enabled_list": enabled_tools,
                "disabled_list": disabled,
            },
            "commands": {
                "allowlist": allowlist,
                "blocklist": blocklist,
            },
            "privacy": {
                "profile": self.state.privacy.profile.to_string(),
                "redaction_enabled": self.state.privacy.redaction_enabled,
            },
            "capacities": {
                "event_log": self.state.event_log.capacity(),
                "eval_timeout_secs": self.state.eval_timeout.as_secs(),
            },
            "registered_commands": self.state.registry.count(),
            "tool_invocations": self.state.tool_invocations.load(std::sync::atomic::Ordering::Relaxed),
            "uptime_secs": self.state.started_at.elapsed().as_secs(),
        });
        json_result(&result)
    }

    #[tool(
        description = "Run environment diagnostics: detect service workers (break IPC interception), closed shadow DOM (invisible to snapshots), iframes (bridge absent), large DOM warnings, and CSP status. Call this first when connecting to an unfamiliar app.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_diagnostics(
        &self,
        Parameters(params): Parameters<DiagnosticsParams>,
    ) -> CallToolResult {
        self.eval_bridge(
            "return window.__VICTAURI__?.getDiagnostics()",
            params.webview_label.as_deref(),
        )
        .await
    }

    // ── Backend Access Tools ───────────────────────────────────────────────

    #[tool(
        description = "Get comprehensive app info: Tauri config (identifier, product name, version), app directory paths (data, config, log, local_data), process environment variables, and database files found in app directories. Provides direct backend context without going through the webview.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn app_info(&self) -> CallToolResult {
        self.track_tool_call();
        let config = self.bridge.tauri_config();

        let data_dir = self.bridge.app_data_dir().ok();
        let config_dir = self.bridge.app_config_dir().ok();
        let log_dir = self.bridge.app_log_dir().ok();
        let local_data_dir = self.bridge.app_local_data_dir().ok();

        let env_vars: std::collections::BTreeMap<String, String> = std::env::vars()
            .filter(|(k, _)| {
                let upper = k.to_uppercase();
                SAFE_ENV_PREFIXES
                    .iter()
                    .any(|prefix| upper.starts_with(prefix))
            })
            .collect();

        #[cfg(feature = "sqlite")]
        let databases: Vec<String> = data_dir
            .as_ref()
            .map(|d| {
                crate::database::discover_databases(d)
                    .into_iter()
                    .filter_map(|p| {
                        p.strip_prefix(d)
                            .ok()
                            .map(|rel| rel.to_string_lossy().into_owned())
                    })
                    .collect()
            })
            .unwrap_or_default();

        #[cfg(not(feature = "sqlite"))]
        let databases: Vec<String> = Vec::new();

        let result = serde_json::json!({
            "config": config,
            "paths": {
                "data": data_dir.as_ref().map(|p| p.to_string_lossy()),
                "config": config_dir.as_ref().map(|p| p.to_string_lossy()),
                "log": log_dir.as_ref().map(|p| p.to_string_lossy()),
                "local_data": local_data_dir.as_ref().map(|p| p.to_string_lossy()),
            },
            "databases": databases,
            "env": env_vars,
            "process": {
                "pid": std::process::id(),
                "arch": std::env::consts::ARCH,
                "os": std::env::consts::OS,
                "family": std::env::consts::FAMILY,
            },
        });
        json_result(&result)
    }

    #[tool(
        description = "List files in the app's data, config, log, or local_data directories. Useful for discovering databases, config files, logs, and cached data on the backend — without going through the webview.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_app_dir(
        &self,
        Parameters(params): Parameters<ListAppDirParams>,
    ) -> CallToolResult {
        self.track_tool_call();
        let base = match self.resolve_app_dir(params.directory) {
            Ok(d) => d,
            Err(e) => return tool_error(e),
        };

        let target = if let Some(ref sub) = params.path {
            let resolved = base.join(sub);
            if !resolved.exists() {
                return tool_error(format!("directory does not exist: {}", resolved.display()));
            }
            if let Err(e) = Self::safe_within(&base, &resolved) {
                return tool_error(e);
            }
            resolved
        } else {
            base.clone()
        };

        if !target.exists() {
            return tool_error(format!("directory does not exist: {}", target.display()));
        }

        let max_depth = params.max_depth.unwrap_or(1).min(5);
        let pattern = params.pattern.as_deref();
        let mut entries = Vec::new();

        Self::list_dir_recursive(&target, &base, 0, max_depth, pattern, &mut entries);

        json_result(&serde_json::json!({
            "base": base.to_string_lossy(),
            "path": params.path.unwrap_or_default(),
            "entries": entries,
            "count": entries.len(),
        }))
    }

    #[tool(
        description = "Read a file from the app's data, config, log, or local_data directory. Returns UTF-8 text by default, or base64 for binary files. Directly reads backend files without going through the webview.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn read_app_file(
        &self,
        Parameters(params): Parameters<ReadAppFileParams>,
    ) -> CallToolResult {
        self.track_tool_call();
        let base = match self.resolve_app_dir(params.directory) {
            Ok(d) => d,
            Err(e) => return tool_error(e),
        };

        let target = base.join(&params.path);
        if !target.exists() {
            return tool_error(format!("file not found: {}", params.path));
        }
        if let Err(e) = Self::safe_within(&base, &target) {
            return tool_error(e);
        }
        if !target.is_file() {
            return tool_error(format!("not a file: {}", params.path));
        }

        let max_bytes = params.max_bytes.unwrap_or(1_048_576).min(10_485_760);
        let metadata = std::fs::metadata(&target).map_err(|e| e.to_string());

        match std::fs::read(&target) {
            Ok(mut bytes) => {
                let original_size = bytes.len();
                let truncated = bytes.len() > max_bytes;
                if truncated {
                    bytes.truncate(max_bytes);
                }

                let file_info = serde_json::json!({
                    "path": params.path,
                    "size": original_size,
                    "truncated": truncated,
                    "modified": metadata.as_ref().ok()
                        .and_then(|m| m.modified().ok())
                        .map(|t| {
                            let duration = t.duration_since(std::time::SystemTime::UNIX_EPOCH).unwrap_or_default();
                            duration.as_secs()
                        }),
                });

                if params.binary == Some(true) {
                    use base64::Engine;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    json_result(&serde_json::json!({
                        "file": file_info,
                        "encoding": "base64",
                        "content": b64,
                    }))
                } else {
                    match String::from_utf8(bytes) {
                        Ok(text) => json_result(&serde_json::json!({
                            "file": file_info,
                            "encoding": "utf-8",
                            "content": text,
                        })),
                        Err(e) => {
                            use base64::Engine;
                            let bytes = e.into_bytes();
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                            json_result(&serde_json::json!({
                                "file": file_info,
                                "encoding": "base64",
                                "note": "file is not valid UTF-8, returning base64",
                                "content": b64,
                            }))
                        }
                    }
                }
            }
            Err(e) => tool_error(format!("failed to read file: {e}")),
        }
    }

    #[cfg(feature = "sqlite")]
    #[tool(
        description = "Execute a read-only SQL query against a SQLite database in the app's data directory. Auto-discovers database files if no path is specified. Only SELECT/PRAGMA/EXPLAIN/WITH queries are allowed. Returns rows as JSON objects with column names as keys. This provides direct backend database access without going through the webview or IPC.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn query_db(&self, Parameters(params): Parameters<QueryDbParams>) -> CallToolResult {
        self.track_tool_call();
        let data_dir = match self.bridge.app_data_dir() {
            Ok(d) => d,
            Err(e) => return tool_error(format!("cannot access app data directory: {e}")),
        };

        let db_path = if let Some(ref rel_path) = params.path {
            let resolved = data_dir.join(rel_path);
            if !resolved.exists() {
                return tool_error(format!("database not found: {rel_path}"));
            }
            if let Err(e) = Self::safe_within(&data_dir, &resolved) {
                return tool_error(e);
            }
            resolved
        } else {
            let databases = crate::database::discover_databases(&data_dir);
            match databases.first() {
                Some(p) => p.clone(),
                None => {
                    return tool_error(format!(
                        "no SQLite databases found in {}",
                        data_dir.display()
                    ));
                }
            }
        };

        let db_display = db_path
            .strip_prefix(&data_dir)
            .unwrap_or(&db_path)
            .to_string_lossy()
            .into_owned();
        let bind_params = params.params.unwrap_or_default();

        match crate::database::query(&db_path, &params.query, &bind_params, params.max_rows) {
            Ok(mut result) => {
                if let Some(obj) = result.as_object_mut() {
                    obj.insert("database".to_string(), serde_json::json!(db_display));
                }
                json_result(&result)
            }
            Err(e) => tool_error(e),
        }
    }

    // ── Compound Tools ──────────────────────────────────────────────────────

    #[tool(
        description = "DOM element interactions. Actions: click, double_click, hover, focus, scroll_into_view, select_option. Requires ref_id from a dom_snapshot for most actions.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn interact(&self, Parameters(params): Parameters<InteractParams>) -> CallToolResult {
        if !self.state.privacy.is_tool_enabled("interact") {
            return tool_disabled("interact");
        }
        match params.action {
            InteractAction::Click => {
                if !self.state.privacy.is_tool_enabled("interact.click") {
                    return tool_disabled("interact.click");
                }
                let Some(ref_id) = &params.ref_id else {
                    return missing_param("ref_id", "click");
                };
                let code = format!("return window.__VICTAURI__?.click({})", js_string(ref_id));
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            InteractAction::DoubleClick => {
                if !self.state.privacy.is_tool_enabled("interact.double_click") {
                    return tool_disabled("interact.double_click");
                }
                let Some(ref_id) = &params.ref_id else {
                    return missing_param("ref_id", "double_click");
                };
                let code = format!(
                    "return window.__VICTAURI__?.doubleClick({})",
                    js_string(ref_id)
                );
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            InteractAction::Hover => {
                if !self.state.privacy.is_tool_enabled("interact.hover") {
                    return tool_disabled("interact.hover");
                }
                let Some(ref_id) = &params.ref_id else {
                    return missing_param("ref_id", "hover");
                };
                let code = format!("return window.__VICTAURI__?.hover({})", js_string(ref_id));
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            InteractAction::Focus => {
                if !self.state.privacy.is_tool_enabled("interact.focus") {
                    return tool_disabled("interact.focus");
                }
                let Some(ref_id) = &params.ref_id else {
                    return missing_param("ref_id", "focus");
                };
                let code = format!(
                    "return window.__VICTAURI__?.focusElement({})",
                    js_string(ref_id)
                );
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            InteractAction::ScrollIntoView => {
                if !self
                    .state
                    .privacy
                    .is_tool_enabled("interact.scroll_into_view")
                {
                    return tool_disabled("interact.scroll_into_view");
                }
                let ref_arg = params
                    .ref_id
                    .as_ref()
                    .map_or_else(|| "null".to_string(), |r| js_string(r));
                let x = params.x.unwrap_or(0.0);
                let y = params.y.unwrap_or(0.0);
                let code = format!("return window.__VICTAURI__?.scrollTo({ref_arg}, {x}, {y})");
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            InteractAction::SelectOption => {
                if !self.state.privacy.is_tool_enabled("interact.select_option") {
                    return tool_disabled("interact.select_option");
                }
                let Some(ref_id) = &params.ref_id else {
                    return missing_param("ref_id", "select_option");
                };
                let values = params.values.as_deref().unwrap_or(&[]);
                let values_json =
                    serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string());
                let code = format!(
                    "return window.__VICTAURI__?.selectOption({}, {})",
                    js_string(ref_id),
                    values_json
                );
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
        }
    }

    #[tool(
        description = "Text and keyboard input. Actions: fill (set input value), type_text (character-by-character typing), press_key (trigger a keyboard key). Subject to privacy controls.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn input(&self, Parameters(params): Parameters<InputParams>) -> CallToolResult {
        match params.action {
            InputAction::Fill => {
                if !self.state.privacy.is_tool_enabled("fill") {
                    return tool_disabled("fill");
                }
                let Some(ref_id) = &params.ref_id else {
                    return missing_param("ref_id", "fill");
                };
                let Some(value) = &params.value else {
                    return missing_param("value", "fill");
                };
                let code = format!(
                    "return window.__VICTAURI__?.fill({}, {})",
                    js_string(ref_id),
                    js_string(value)
                );
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            InputAction::TypeText => {
                if !self.state.privacy.is_tool_enabled("type_text") {
                    return tool_disabled("type_text");
                }
                let Some(ref_id) = &params.ref_id else {
                    return missing_param("ref_id", "type_text");
                };
                let Some(text) = &params.text else {
                    return missing_param("text", "type_text");
                };
                let code = format!(
                    "return window.__VICTAURI__?.type({}, {})",
                    js_string(ref_id),
                    js_string(text)
                );
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            InputAction::PressKey => {
                if !self.state.privacy.is_tool_enabled("input.press_key") {
                    return tool_disabled("input.press_key");
                }
                let Some(key) = &params.key else {
                    return missing_param("key", "press_key");
                };
                let code = format!("return window.__VICTAURI__?.pressKey({})", js_string(key));
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
        }
    }

    #[tool(
        description = "Window management. Actions: get_state (window positions/sizes/visibility), list (all window labels), manage (minimize/maximize/close/focus/show/hide/fullscreen/always_on_top), resize, move_to, set_title.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn window(&self, Parameters(params): Parameters<WindowParams>) -> CallToolResult {
        self.track_tool_call();
        match params.action {
            WindowAction::GetState => {
                let states = self.bridge.get_window_states(params.label.as_deref());
                json_result(&states)
            }
            WindowAction::List => {
                let labels = self.bridge.list_window_labels();
                json_result(&labels)
            }
            WindowAction::Manage => {
                if !self.state.privacy.is_tool_enabled("window.manage") {
                    return tool_disabled("window.manage");
                }
                let Some(manage_action) = &params.manage_action else {
                    return missing_param("manage_action", "manage");
                };
                match self
                    .bridge
                    .manage_window(params.label.as_deref(), manage_action.as_str())
                {
                    Ok(msg) => CallToolResult::success(vec![Content::text(msg)]),
                    Err(e) => tool_error(e),
                }
            }
            WindowAction::Resize => {
                if !self.state.privacy.is_tool_enabled("window.resize") {
                    return tool_disabled("window.resize");
                }
                let Some(width) = params.width else {
                    return missing_param("width", "resize");
                };
                let Some(height) = params.height else {
                    return missing_param("height", "resize");
                };
                match self
                    .bridge
                    .resize_window(params.label.as_deref(), width, height)
                {
                    Ok(()) => {
                        let result =
                            serde_json::json!({"ok": true, "width": width, "height": height});
                        CallToolResult::success(vec![Content::text(result.to_string())])
                    }
                    Err(e) => tool_error(e),
                }
            }
            WindowAction::MoveTo => {
                if !self.state.privacy.is_tool_enabled("window.move_to") {
                    return tool_disabled("window.move_to");
                }
                let Some(x) = params.x else {
                    return missing_param("x", "move_to");
                };
                let Some(y) = params.y else {
                    return missing_param("y", "move_to");
                };
                match self.bridge.move_window(params.label.as_deref(), x, y) {
                    Ok(()) => {
                        let result = serde_json::json!({"ok": true, "x": x, "y": y});
                        CallToolResult::success(vec![Content::text(result.to_string())])
                    }
                    Err(e) => tool_error(e),
                }
            }
            WindowAction::SetTitle => {
                if !self.state.privacy.is_tool_enabled("window.set_title") {
                    return tool_disabled("window.set_title");
                }
                let Some(title) = &params.title else {
                    return missing_param("title", "set_title");
                };
                match self.bridge.set_window_title(params.label.as_deref(), title) {
                    Ok(()) => {
                        let result = serde_json::json!({"ok": true, "title": title});
                        CallToolResult::success(vec![Content::text(result.to_string())])
                    }
                    Err(e) => tool_error(e),
                }
            }
        }
    }

    #[tool(
        description = "Browser storage operations. Actions: get (read localStorage/sessionStorage), set (write), delete (remove key), get_cookies. Subject to privacy controls for set and delete.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn storage(&self, Parameters(params): Parameters<StorageParams>) -> CallToolResult {
        match params.action {
            StorageAction::Get => {
                let method = match params.storage_type.unwrap_or(StorageType::Local) {
                    StorageType::Session => "getSessionStorage",
                    StorageType::Local => "getLocalStorage",
                };
                let key_arg = params
                    .key
                    .as_ref()
                    .map(|k| js_string(k))
                    .unwrap_or_default();
                let code = format!("return window.__VICTAURI__?.{method}({key_arg})");
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            StorageAction::Set => {
                if !self.state.privacy.is_tool_enabled("set_storage") {
                    return tool_disabled("set_storage");
                }
                let method = match params.storage_type.unwrap_or(StorageType::Local) {
                    StorageType::Session => "setSessionStorage",
                    StorageType::Local => "setLocalStorage",
                };
                let Some(key) = &params.key else {
                    return missing_param("key", "set");
                };
                let value = params
                    .value
                    .as_ref()
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let value_json =
                    serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string());
                let code = format!(
                    "return window.__VICTAURI__?.{method}({}, {value_json})",
                    js_string(key)
                );
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            StorageAction::Delete => {
                if !self.state.privacy.is_tool_enabled("delete_storage") {
                    return tool_disabled("delete_storage");
                }
                let method = match params.storage_type.unwrap_or(StorageType::Local) {
                    StorageType::Session => "deleteSessionStorage",
                    StorageType::Local => "deleteLocalStorage",
                };
                let Some(key) = &params.key else {
                    return missing_param("key", "delete");
                };
                let code = format!("return window.__VICTAURI__?.{method}({})", js_string(key));
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            StorageAction::GetCookies => {
                self.eval_bridge(
                    "return window.__VICTAURI__?.getCookies()",
                    params.webview_label.as_deref(),
                )
                .await
            }
        }
    }

    #[tool(
        description = "Navigation and dialog control. Actions: go_to (navigate to URL), go_back (browser back), get_history (navigation log), set_dialog_response (auto-respond to alert/confirm/prompt), get_dialog_log (captured dialog events). Subject to privacy controls for go_to and set_dialog_response.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn navigate(&self, Parameters(params): Parameters<NavigateParams>) -> CallToolResult {
        match params.action {
            NavigateAction::GoTo => {
                if !self.state.privacy.is_tool_enabled("navigate") {
                    return tool_disabled("navigate");
                }
                let Some(url) = &params.url else {
                    return missing_param("url", "go_to");
                };
                if let Err(e) = validate_url(url, self.state.allow_file_navigation) {
                    return tool_error(e);
                }
                let code = format!("return window.__VICTAURI__?.navigate({})", js_string(url));
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            NavigateAction::GoBack => {
                self.eval_bridge(
                    "return window.__VICTAURI__?.navigateBack()",
                    params.webview_label.as_deref(),
                )
                .await
            }
            NavigateAction::GetHistory => {
                self.eval_bridge(
                    "return window.__VICTAURI__?.getNavigationLog()",
                    params.webview_label.as_deref(),
                )
                .await
            }
            NavigateAction::SetDialogResponse => {
                if !self.state.privacy.is_tool_enabled("set_dialog_response") {
                    return tool_disabled("set_dialog_response");
                }
                let Some(dialog_type) = params.dialog_type else {
                    return missing_param("dialog_type", "set_dialog_response");
                };
                let Some(dialog_action) = params.dialog_action else {
                    return missing_param("dialog_action", "set_dialog_response");
                };
                let text_arg = params
                    .text
                    .as_ref()
                    .map_or_else(|| "undefined".to_string(), |t| js_string(t));
                let code = format!(
                    "return window.__VICTAURI__?.setDialogAutoResponse({}, {}, {text_arg})",
                    js_string(dialog_type.as_str()),
                    js_string(dialog_action.as_str())
                );
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            NavigateAction::GetDialogLog => {
                self.eval_bridge(
                    "return window.__VICTAURI__?.getDialogLog()",
                    params.webview_label.as_deref(),
                )
                .await
            }
        }
    }

    #[tool(
        description = "Time-travel recording. Actions: start (begin recording), stop (end and return session), checkpoint (save state snapshot), list_checkpoints, get_events (since index), events_between (two checkpoints), get_replay (IPC replay sequence), export (session as JSON), import (load session from JSON).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn recording(&self, Parameters(params): Parameters<RecordingParams>) -> CallToolResult {
        const MAX_SESSION_JSON: usize = 10 * 1024 * 1024;
        self.track_tool_call();
        if !self.state.privacy.is_tool_enabled("recording") {
            return tool_disabled("recording");
        }
        match params.action {
            RecordingAction::Start => {
                let session_id = params
                    .session_id
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                match self.state.recorder.start(session_id.clone()) {
                    Ok(()) => {
                        let result = serde_json::json!({
                            "started": true,
                            "session_id": session_id,
                        });
                        CallToolResult::success(vec![Content::text(result.to_string())])
                    }
                    Err(e) => tool_error(e.to_string()),
                }
            }
            RecordingAction::Stop => match self.state.recorder.stop() {
                Some(session) => json_result(&session),
                None => tool_error("no recording is active"),
            },
            RecordingAction::Checkpoint => {
                let Some(id) = params.checkpoint_id else {
                    return missing_param("checkpoint_id", "checkpoint");
                };
                let state = params.state.unwrap_or(serde_json::Value::Null);
                match self
                    .state
                    .recorder
                    .checkpoint(id.clone(), params.checkpoint_label, state)
                {
                    Ok(()) => {
                        let result = serde_json::json!({
                            "created": true,
                            "checkpoint_id": id,
                            "event_index": self.state.recorder.event_count(),
                        });
                        CallToolResult::success(vec![Content::text(result.to_string())])
                    }
                    Err(e) => tool_error(e.to_string()),
                }
            }
            RecordingAction::ListCheckpoints => {
                let checkpoints = self.state.recorder.get_checkpoints();
                json_result(&checkpoints)
            }
            RecordingAction::GetEvents => {
                let events = self
                    .state
                    .recorder
                    .events_since(params.since_index.unwrap_or(0));
                json_result(&events)
            }
            RecordingAction::EventsBetween => {
                let Some(from) = &params.from else {
                    return missing_param("from", "events_between");
                };
                let Some(to) = &params.to else {
                    return missing_param("to", "events_between");
                };
                match self.state.recorder.events_between_checkpoints(from, to) {
                    Ok(events) => json_result(&events),
                    Err(e) => tool_error(e.to_string()),
                }
            }
            RecordingAction::GetReplay => {
                let calls = self.state.recorder.ipc_replay_sequence();
                json_result(&calls)
            }
            RecordingAction::Export => match self.state.recorder.export() {
                Some(s) => {
                    let json = serde_json::to_string_pretty(&s)
                        .unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"));
                    CallToolResult::success(vec![Content::text(json)])
                }
                None => tool_error("no recording is active — start one first"),
            },
            RecordingAction::Import => {
                let Some(session_json) = &params.session_json else {
                    return missing_param("session_json", "import");
                };
                if session_json.len() > MAX_SESSION_JSON {
                    return tool_error("session JSON exceeds maximum size (10 MB)");
                }
                let session: victauri_core::RecordedSession =
                    match serde_json::from_str(session_json) {
                        Ok(s) => s,
                        Err(e) => return tool_error(format!("invalid session JSON: {e}")),
                    };

                let result = serde_json::json!({
                    "imported": true,
                    "session_id": session.id,
                    "event_count": session.events.len(),
                    "checkpoint_count": session.checkpoints.len(),
                    "started_at": session.started_at.to_rfc3339(),
                });
                self.state.recorder.import(session);
                CallToolResult::success(vec![Content::text(result.to_string())])
            }
        }
    }

    #[tool(
        description = "CSS and visual inspection. Actions: get_styles (computed CSS for element), get_bounding_boxes (layout rects), highlight (debug overlay), clear_highlights, audit_accessibility (a11y audit), get_performance (timing/heap/DOM metrics).",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn inspect(&self, Parameters(params): Parameters<InspectParams>) -> CallToolResult {
        match params.action {
            InspectAction::GetStyles => {
                let Some(ref_id) = &params.ref_id else {
                    return missing_param("ref_id", "get_styles");
                };
                let props_arg = match &params.properties {
                    Some(props) => {
                        let arr: Vec<String> = props.iter().map(|p| js_string(p)).collect();
                        format!("[{}]", arr.join(","))
                    }
                    None => "null".to_string(),
                };
                let code = format!(
                    "return window.__VICTAURI__?.getStyles({}, {})",
                    js_string(ref_id),
                    props_arg
                );
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            InspectAction::GetBoundingBoxes => {
                let Some(ref_ids) = &params.ref_ids else {
                    return missing_param("ref_ids", "get_bounding_boxes");
                };
                let refs: Vec<String> = ref_ids.iter().map(|r| js_string(r)).collect();
                let code = format!(
                    "return window.__VICTAURI__?.getBoundingBoxes([{}])",
                    refs.join(",")
                );
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            InspectAction::Highlight => {
                let Some(ref_id) = &params.ref_id else {
                    return missing_param("ref_id", "highlight");
                };
                let color_arg = match &params.color {
                    Some(c) => match sanitize_css_color(c) {
                        Ok(safe) => format!("\"{safe}\""),
                        Err(e) => return tool_error(e),
                    },
                    None => "null".to_string(),
                };
                let label_arg = match &params.label {
                    Some(l) => js_string(l),
                    None => "null".to_string(),
                };
                let code = format!(
                    "return window.__VICTAURI__?.highlightElement({}, {}, {})",
                    js_string(ref_id),
                    color_arg,
                    label_arg
                );
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            InspectAction::ClearHighlights => {
                self.eval_bridge(
                    "return window.__VICTAURI__?.clearHighlights()",
                    params.webview_label.as_deref(),
                )
                .await
            }
            InspectAction::AuditAccessibility => {
                self.eval_bridge(
                    "return window.__VICTAURI__?.auditAccessibility()",
                    params.webview_label.as_deref(),
                )
                .await
            }
            InspectAction::GetPerformance => {
                self.eval_bridge(
                    "return window.__VICTAURI__?.getPerformanceMetrics()",
                    params.webview_label.as_deref(),
                )
                .await
            }
        }
    }

    #[tool(
        description = "CSS injection. Actions: inject (add custom CSS to page), remove (remove previously injected CSS). Subject to privacy controls.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn css(&self, Parameters(params): Parameters<CssParams>) -> CallToolResult {
        match params.action {
            CssAction::Inject => {
                if !self.state.privacy.is_tool_enabled("inject_css") {
                    return tool_disabled("inject_css");
                }
                let Some(css) = &params.css else {
                    return missing_param("css", "inject");
                };
                let code = format!("return window.__VICTAURI__?.injectCss({})", js_string(css));
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            CssAction::Remove => {
                if !self.state.privacy.is_tool_enabled("css.remove") {
                    return tool_disabled("css.remove");
                }
                self.eval_bridge(
                    "return window.__VICTAURI__?.removeInjectedCss()",
                    params.webview_label.as_deref(),
                )
                .await
            }
        }
    }

    #[tool(
        description = "Application logs and monitoring. Actions: console (captured console.log/warn/error), network (intercepted fetch/XHR), ipc (IPC call log — set wait_for_capture=true to await response capture up to 500ms), navigation (URL change history), dialogs (alert/confirm/prompt events), events (combined event stream), slow_ipc (find slow IPC calls).",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn logs(&self, Parameters(params): Parameters<LogsParams>) -> CallToolResult {
        match params.action {
            LogsAction::Console => {
                let since_arg = params.since.map(|ts| format!("{ts}")).unwrap_or_default();
                let code = if since_arg.is_empty() {
                    "return window.__VICTAURI__?.getConsoleLogs()".to_string()
                } else {
                    format!("return window.__VICTAURI__?.getConsoleLogs({since_arg})")
                };
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            LogsAction::Network => {
                let filter_arg = params
                    .filter
                    .as_ref()
                    .map_or_else(|| "null".to_string(), |f| js_string(f));
                let limit_arg = params
                    .limit
                    .map_or_else(|| "null".to_string(), |l| l.to_string());
                let code =
                    format!("return window.__VICTAURI__?.getNetworkLog({filter_arg}, {limit_arg})");
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            LogsAction::Ipc => {
                let wait = params.wait_for_capture.unwrap_or(false);
                let limit_arg = params.limit.map(|l| format!("{l}")).unwrap_or_default();
                if wait {
                    let limit_js = if limit_arg.is_empty() {
                        "undefined".to_string()
                    } else {
                        limit_arg.clone()
                    };
                    let code = format!(
                        r"return (async function() {{
                            await window.__VICTAURI__.waitForIpcComplete(500);
                            var log = window.__VICTAURI__.getIpcLog() || [];
                            var lim = {limit_js};
                            return (lim !== undefined) ? log.slice(-lim) : log;
                        }})()"
                    );
                    let timeout = std::time::Duration::from_millis(5000);
                    match self
                        .eval_with_return_timeout(&code, params.webview_label.as_deref(), timeout)
                        .await
                    {
                        Ok(result) => CallToolResult::success(vec![Content::text(result)]),
                        Err(e) => tool_error(e),
                    }
                } else {
                    let code = if limit_arg.is_empty() {
                        "return window.__VICTAURI__?.getIpcLog()".to_string()
                    } else {
                        format!("return window.__VICTAURI__?.getIpcLog({limit_arg})")
                    };
                    self.eval_bridge(&code, params.webview_label.as_deref())
                        .await
                }
            }
            LogsAction::Navigation => {
                self.eval_bridge(
                    "return window.__VICTAURI__?.getNavigationLog()",
                    params.webview_label.as_deref(),
                )
                .await
            }
            LogsAction::Dialogs => {
                self.eval_bridge(
                    "return window.__VICTAURI__?.getDialogLog()",
                    params.webview_label.as_deref(),
                )
                .await
            }
            LogsAction::Events => {
                let since_arg = params.since.map(|ts| format!("{ts}")).unwrap_or_default();
                let code = if since_arg.is_empty() {
                    "return window.__VICTAURI__?.getEventStream()".to_string()
                } else {
                    format!("return window.__VICTAURI__?.getEventStream({since_arg})")
                };
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            LogsAction::SlowIpc => {
                let Some(threshold) = params.threshold_ms else {
                    return missing_param("threshold_ms", "slow_ipc");
                };
                let limit = params.limit.unwrap_or(20);
                let code = format!(
                    r"return (function() {{
                        var log = window.__VICTAURI__?.getIpcLog() || [];
                        var slow = log.filter(function(c) {{ return (c.duration_ms || 0) > {threshold}; }});
                        slow.sort(function(a, b) {{ return (b.duration_ms || 0) - (a.duration_ms || 0); }});
                        return {{ threshold_ms: {threshold}, count: Math.min(slow.length, {limit}), calls: slow.slice(0, {limit}) }};
                    }})()",
                );
                self.eval_bridge(&code, None).await
            }
        }
    }
}

impl VictauriMcpHandler {
    /// Create a new handler backed by the given state and webview bridge.
    pub fn new(state: Arc<VictauriState>, bridge: Arc<dyn WebviewBridge>) -> Self {
        Self {
            state,
            bridge,
            subscriptions: Arc::new(Mutex::new(HashSet::new())),
            bridge_checked: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn is_tool_enabled(&self, name: &str) -> bool {
        self.state.privacy.is_tool_enabled(name)
    }

    pub(crate) async fn execute_tool(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<CallToolResult, rest::ToolCallError> {
        if !self.state.privacy.is_tool_enabled(name) {
            return Ok(tool_disabled(name));
        }
        self.state.tool_invocations.fetch_add(1, Ordering::Relaxed);
        let start = std::time::Instant::now();
        tracing::debug!(tool = %name, "REST tool invocation started");

        let result = match name {
            "eval_js" => {
                let p: EvalJsParams = Self::parse_args(args)?;
                self.eval_js(Parameters(p)).await
            }
            "dom_snapshot" => {
                let p: SnapshotParams = Self::parse_args(args)?;
                self.dom_snapshot(Parameters(p)).await
            }
            "find_elements" => {
                let p: FindElementsParams = Self::parse_args(args)?;
                self.find_elements(Parameters(p)).await
            }
            "invoke_command" => {
                let p: InvokeCommandParams = Self::parse_args(args)?;
                self.invoke_command(Parameters(p)).await
            }
            "screenshot" => {
                let p: ScreenshotParams = Self::parse_args(args)?;
                self.screenshot(Parameters(p)).await
            }
            "verify_state" => {
                let p: VerifyStateParams = Self::parse_args(args)?;
                self.verify_state(Parameters(p)).await
            }
            "detect_ghost_commands" => {
                let p: GhostCommandParams = Self::parse_args(args)?;
                self.detect_ghost_commands(Parameters(p)).await
            }
            "check_ipc_integrity" => {
                let p: IpcIntegrityParams = Self::parse_args(args)?;
                self.check_ipc_integrity(Parameters(p)).await
            }
            "wait_for" => {
                let p: WaitForParams = Self::parse_args(args)?;
                self.wait_for(Parameters(p)).await
            }
            "assert_semantic" => {
                let p: SemanticAssertParams = Self::parse_args(args)?;
                self.assert_semantic(Parameters(p)).await
            }
            "resolve_command" => {
                let p: ResolveCommandParams = Self::parse_args(args)?;
                self.resolve_command(Parameters(p)).await
            }
            "get_registry" => {
                let p: RegistryParams = Self::parse_args(args)?;
                self.get_registry(Parameters(p)).await
            }
            "get_memory_stats" => self.get_memory_stats().await,
            "get_plugin_info" => self.get_plugin_info().await,
            "get_diagnostics" => {
                let p: DiagnosticsParams = Self::parse_args(args)?;
                self.get_diagnostics(Parameters(p)).await
            }
            "app_info" => self.app_info().await,
            "list_app_dir" => {
                let p: ListAppDirParams = Self::parse_args(args)?;
                self.list_app_dir(Parameters(p)).await
            }
            "read_app_file" => {
                let p: ReadAppFileParams = Self::parse_args(args)?;
                self.read_app_file(Parameters(p)).await
            }
            #[cfg(feature = "sqlite")]
            "query_db" => {
                let p: QueryDbParams = Self::parse_args(args)?;
                self.query_db(Parameters(p)).await
            }
            "interact" => {
                let p: InteractParams = Self::parse_args(args)?;
                self.interact(Parameters(p)).await
            }
            "input" => {
                let p: InputParams = Self::parse_args(args)?;
                self.input(Parameters(p)).await
            }
            "window" => {
                let p: WindowParams = Self::parse_args(args)?;
                self.window(Parameters(p)).await
            }
            "storage" => {
                let p: StorageParams = Self::parse_args(args)?;
                self.storage(Parameters(p)).await
            }
            "navigate" => {
                let p: NavigateParams = Self::parse_args(args)?;
                self.navigate(Parameters(p)).await
            }
            "recording" => {
                let p: RecordingParams = Self::parse_args(args)?;
                self.recording(Parameters(p)).await
            }
            "inspect" => {
                let p: InspectParams = Self::parse_args(args)?;
                self.inspect(Parameters(p)).await
            }
            "css" => {
                let p: CssParams = Self::parse_args(args)?;
                self.css(Parameters(p)).await
            }
            "logs" => {
                let p: LogsParams = Self::parse_args(args)?;
                self.logs(Parameters(p)).await
            }
            _ => return Err(rest::ToolCallError::UnknownTool(name.to_string())),
        };

        let elapsed = start.elapsed();
        tracing::debug!(
            tool = %name,
            elapsed_ms = elapsed.as_millis() as u64,
            "REST tool invocation completed"
        );

        if self.state.privacy.redaction_enabled {
            Ok(Self::redact_result(result, &self.state.privacy))
        } else {
            Ok(result)
        }
    }

    fn parse_args<T: serde::de::DeserializeOwned>(
        args: serde_json::Value,
    ) -> Result<T, rest::ToolCallError> {
        serde_json::from_value(args).map_err(|e| rest::ToolCallError::InvalidParams(e.to_string()))
    }

    fn redact_result(
        mut result: CallToolResult,
        privacy: &crate::privacy::PrivacyConfig,
    ) -> CallToolResult {
        for item in &mut result.content {
            if let RawContent::Text(ref mut tc) = item.raw {
                tc.text = privacy.redact_output(&tc.text);
            }
        }
        result
    }

    fn track_tool_call(&self) {
        self.state.tool_invocations.fetch_add(1, Ordering::Relaxed);
    }

    fn resolve_app_dir(&self, dir: Option<AppDir>) -> Result<std::path::PathBuf, String> {
        match dir.unwrap_or(AppDir::Data) {
            AppDir::Data => self.bridge.app_data_dir(),
            AppDir::Config => self.bridge.app_config_dir(),
            AppDir::Log => self.bridge.app_log_dir(),
            AppDir::LocalData => self.bridge.app_local_data_dir(),
        }
    }

    fn safe_within(base: &std::path::Path, target: &std::path::Path) -> Result<(), String> {
        let canon_base = std::fs::canonicalize(base)
            .map_err(|e| format!("cannot resolve base directory: {e}"))?;
        let canon_target = std::fs::canonicalize(target)
            .map_err(|e| format!("cannot resolve target path: {e}"))?;
        if !canon_target.starts_with(&canon_base) {
            return Err("path traversal not allowed".to_string());
        }
        Ok(())
    }

    fn list_dir_recursive(
        dir: &std::path::Path,
        base: &std::path::Path,
        depth: u32,
        max_depth: u32,
        pattern: Option<&str>,
        entries: &mut Vec<serde_json::Value>,
    ) {
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();

            if let Some(pat) = pattern
                && !Self::matches_glob(&name, pat)
                && !path.is_dir()
            {
                continue;
            }

            let is_dir = path.is_dir();
            let meta = std::fs::metadata(&path).ok();

            entries.push(serde_json::json!({
                "name": name,
                "path": relative,
                "is_dir": is_dir,
                "size": meta.as_ref().map(std::fs::Metadata::len),
                "modified": meta.as_ref()
                    .and_then(|m| m.modified().ok())
                    .map(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .unwrap_or_default().as_secs()),
            }));

            if is_dir && depth < max_depth {
                Self::list_dir_recursive(&path, base, depth + 1, max_depth, pattern, entries);
            }
        }
    }

    fn matches_glob(name: &str, pattern: &str) -> bool {
        if pattern == "*" {
            return true;
        }
        if let Some(suffix) = pattern.strip_prefix("*.") {
            return name.ends_with(&format!(".{suffix}"));
        }
        if let Some(prefix) = pattern.strip_suffix("*") {
            return name.starts_with(prefix);
        }
        name == pattern
    }

    async fn eval_bridge(&self, code: &str, webview_label: Option<&str>) -> CallToolResult {
        match self.eval_with_return(code, webview_label).await {
            Ok(result) => CallToolResult::success(vec![Content::text(result)]),
            Err(e) => tool_error(e),
        }
    }

    async fn eval_with_return(
        &self,
        code: &str,
        webview_label: Option<&str>,
    ) -> Result<String, String> {
        self.eval_with_return_timeout(code, webview_label, self.state.eval_timeout)
            .await
    }

    async fn eval_with_return_timeout(
        &self,
        code: &str,
        webview_label: Option<&str>,
        timeout: std::time::Duration,
    ) -> Result<String, String> {
        self.track_tool_call();
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();

        {
            let mut pending = self.state.pending_evals.lock().await;
            if pending.len() >= MAX_PENDING_EVALS {
                return Err(format!(
                    "too many concurrent eval requests (limit: {MAX_PENDING_EVALS})"
                ));
            }
            pending.insert(id.clone(), tx);
        }

        // Auto-prepend `return` so bare expressions produce a value.
        // Only skip for code that starts with a statement keyword where
        // prepending `return` would be a syntax error.
        let code = code.trim();
        let needs_return = !code.starts_with("return ")
            && !code.starts_with("return;")
            && !code.starts_with('{')
            && !code.starts_with("if ")
            && !code.starts_with("if(")
            && !code.starts_with("for ")
            && !code.starts_with("for(")
            && !code.starts_with("while ")
            && !code.starts_with("while(")
            && !code.starts_with("switch ")
            && !code.starts_with("try ")
            && !code.starts_with("const ")
            && !code.starts_with("let ")
            && !code.starts_with("var ")
            && !code.starts_with("function ")
            && !code.starts_with("class ")
            && !code.starts_with("throw ");
        let code = if needs_return {
            format!("return {code}")
        } else {
            code.to_string()
        };

        let id_js = js_string(&id);
        let inject = format!(
            r"
            (async () => {{
                try {{
                    const __result = await (async () => {{ {code} }})();
                    await window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', {{
                        id: {id_js},
                        result: JSON.stringify(__result)
                    }});
                }} catch (e) {{
                    await window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', {{
                        id: {id_js},
                        result: JSON.stringify({{ __error: e.message }})
                    }});
                }}
            }})();
            "
        );

        if let Err(e) = self.bridge.eval_webview(webview_label, &inject) {
            self.state.pending_evals.lock().await.remove(&id);
            return Err(format!("eval injection failed: {e}"));
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => {
                self.check_bridge_version_once();
                Ok(result)
            }
            Ok(Err(_)) => Err("eval callback channel closed".to_string()),
            Err(_) => {
                self.state.pending_evals.lock().await.remove(&id);
                Err(format!("eval timed out after {}s", timeout.as_secs()))
            }
        }
    }

    fn check_bridge_version_once(&self) {
        if self.bridge_checked.swap(true, Ordering::Relaxed) {
            return;
        }
        let handler = self.clone();
        tokio::spawn(async move {
            match handler
                .eval_with_return_timeout(
                    "window.__VICTAURI__?.version",
                    None,
                    std::time::Duration::from_secs(5),
                )
                .await
            {
                Ok(v) => {
                    let v = v.trim_matches('"');
                    if v == BRIDGE_VERSION {
                        tracing::debug!("Bridge version verified: {v}");
                    } else {
                        tracing::warn!(
                            "Bridge version mismatch: Rust expects {BRIDGE_VERSION}, JS reports {v}"
                        );
                    }
                }
                Err(e) => tracing::debug!("Bridge version check skipped: {e}"),
            }
        });
    }
}

const SERVER_INSTRUCTIONS: &str = "Victauri is a FULL-STACK inspection tool for Tauri applications. \
It provides simultaneous access to three layers: (1) the WEBVIEW (DOM, interactions, JS eval), \
(2) the IPC LAYER (command registry, invoke commands, intercept traffic), and \
(3) the RUST BACKEND (app config, file system, SQLite databases, process memory). \
\n\nBACKEND tools (direct Rust access, no webview needed): \
'app_info' (app config, directory paths, discovered databases, process info), \
'list_app_dir' (browse app data/config/log directories), \
'read_app_file' (read files from app directories), \
'query_db' (read-only SQLite queries with auto-discovery). \
\n\nWEBVIEW tools: \
'interact' (click, hover, focus, scroll, select), 'input' (fill, type_text, press_key), \
'inspect' (get_styles, get_bounding_boxes, highlight, audit_accessibility, get_performance), \
'css' (inject, remove), eval_js, dom_snapshot, find_elements, screenshot. \
\n\nIPC tools: invoke_command, get_registry, detect_ghost_commands, check_ipc_integrity. \
\n\nCOMPOUND tools with an 'action' parameter: \
'window' (get_state, list, manage, resize, move_to, set_title), \
'storage' (get, set, delete, get_cookies), 'navigate' (go_to, go_back, get_history, \
set_dialog_response, get_dialog_log), 'recording' (start, stop, checkpoint, list_checkpoints, \
get_events, events_between, get_replay, export, import), \
'logs' (console, network, ipc, navigation, dialogs, events, slow_ipc). \
\n\nOTHER: verify_state, wait_for, assert_semantic, resolve_command, \
get_memory_stats, get_plugin_info, get_diagnostics.";

impl ServerHandler for VictauriMcpHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_resources_subscribe()
                .build(),
        )
        .with_instructions(SERVER_INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let all_tools = Self::tool_router().list_all();
        let filtered: Vec<Tool> = all_tools
            .into_iter()
            .filter(|t| self.state.privacy.is_tool_enabled(t.name.as_ref()))
            .collect();
        Ok(ListToolsResult {
            tools: filtered,
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let tool_name: String = request.name.as_ref().to_owned();
        if !self.state.privacy.is_tool_enabled(&tool_name) {
            tracing::debug!(tool = %tool_name, "tool call blocked by privacy config");
            return Ok(tool_disabled(&tool_name));
        }
        self.state
            .tool_invocations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let start = std::time::Instant::now();
        tracing::debug!(tool = %tool_name, "tool invocation started");
        let ctx = ToolCallContext::new(self, request, context);
        let result = Self::tool_router().call(ctx).await;
        let elapsed = start.elapsed();
        tracing::debug!(
            tool = %tool_name,
            elapsed_ms = elapsed.as_millis() as u64,
            is_error = result.as_ref().map_or(true, |r| r.is_error.unwrap_or(false)),
            "tool invocation completed"
        );

        // Centralized output redaction: apply to all text content so no
        // individual tool can accidentally leak secrets.
        if self.state.privacy.redaction_enabled {
            result.map(|mut r| {
                for item in &mut r.content {
                    if let RawContent::Text(ref mut tc) = item.raw {
                        tc.text = self.state.privacy.redact_output(&tc.text);
                    }
                }
                r
            })
        } else {
            result
        }
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        if !self.state.privacy.is_tool_enabled(name) {
            return None;
        }
        Self::tool_router().get(name).cloned()
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult {
            resources: vec![
                RawResource::new(RESOURCE_URI_IPC_LOG, "ipc-log")
                    .with_description(
                        "Live IPC call log — all commands invoked between frontend and backend",
                    )
                    .with_mime_type("application/json")
                    .no_annotation(),
                RawResource::new(RESOURCE_URI_WINDOWS, "windows")
                    .with_description(
                        "Current state of all Tauri windows — position, size, visibility, focus",
                    )
                    .with_mime_type("application/json")
                    .no_annotation(),
                RawResource::new(RESOURCE_URI_STATE, "state")
                    .with_description(
                        "Victauri plugin state — event count, registered commands, memory stats",
                    )
                    .with_mime_type("application/json")
                    .no_annotation(),
            ],
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let uri = &request.uri;
        let json = match uri.as_str() {
            RESOURCE_URI_IPC_LOG => {
                if let Ok(json) = self
                    .eval_with_return("return window.__VICTAURI__?.getIpcLog()", None)
                    .await
                {
                    json
                } else {
                    let calls = self.state.event_log.ipc_calls();
                    serde_json::to_string_pretty(&calls)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                }
            }
            RESOURCE_URI_WINDOWS => {
                let states = self.bridge.get_window_states(None);
                serde_json::to_string_pretty(&states)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
            }
            RESOURCE_URI_STATE => {
                let state_json = serde_json::json!({
                    "events_captured": self.state.event_log.len(),
                    "commands_registered": self.state.registry.count(),
                    "memory": crate::memory::current_stats(),
                    "port": self.state.port.load(Ordering::Relaxed),
                });
                serde_json::to_string_pretty(&state_json)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
            }
            _ => {
                return Err(ErrorData::resource_not_found(
                    format!("unknown resource: {uri}"),
                    None,
                ));
            }
        };

        let json = if self.state.privacy.redaction_enabled {
            self.state.privacy.redact_output(&json)
        } else {
            json
        };

        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            json, uri,
        )]))
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let uri = &request.uri;
        match uri.as_str() {
            RESOURCE_URI_IPC_LOG | RESOURCE_URI_WINDOWS | RESOURCE_URI_STATE => {
                self.subscriptions.lock().await.insert(uri.clone());
                tracing::info!("Client subscribed to resource: {uri}");
                Ok(())
            }
            _ => Err(ErrorData::resource_not_found(
                format!("unknown resource: {uri}"),
                None,
            )),
        }
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.subscriptions.lock().await.remove(&request.uri);
        tracing::info!("Client unsubscribed from resource: {}", request.uri);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_string_simple() {
        assert_eq!(js_string("hello"), "\"hello\"");
    }

    #[test]
    fn js_string_single_quotes() {
        let result = js_string("it's a test");
        assert!(result.contains("it's a test"));
    }

    #[test]
    fn js_string_double_quotes() {
        let result = js_string(r#"say "hello""#);
        assert!(result.contains(r#"\""#));
    }

    #[test]
    fn js_string_backslashes() {
        let result = js_string(r"path\to\file");
        assert!(result.contains(r"\\"));
    }

    #[test]
    fn js_string_newlines_and_tabs() {
        let result = js_string("line1\nline2\ttab");
        assert!(result.contains(r"\n"));
        assert!(result.contains(r"\t"));
        assert!(!result.contains('\n'));
    }

    #[test]
    fn js_string_null_bytes() {
        let input = String::from_utf8(b"before\x00after".to_vec()).unwrap();
        let result = js_string(&input);
        // serde_json escapes null bytes as
        assert!(result.contains("\\u0000"));
        assert!(!result.contains('\0'));
    }

    #[test]
    fn js_string_template_literal_injection() {
        let result = js_string("`${alert(1)}`");
        // Should not contain unescaped backticks that could break template literals
        // serde_json wraps in double quotes, so backticks are safe
        assert!(result.starts_with('"'));
        assert!(result.ends_with('"'));
    }

    #[test]
    fn js_string_unicode_separators() {
        // U+2028 (Line Separator) and U+2029 (Paragraph Separator) are valid in
        // JSON strings per RFC 8259, and serde_json passes them through literally.
        // Since js_string is used inside JS double-quoted strings (not template
        // literals), they are safe in modern JS engines (ES2019+).
        let result = js_string("a\u{2028}b\u{2029}c");
        // Verify the string is valid JSON that round-trips correctly
        let decoded: String = serde_json::from_str(&result).unwrap();
        assert_eq!(decoded, "a\u{2028}b\u{2029}c");
    }

    #[test]
    fn js_string_empty() {
        assert_eq!(js_string(""), "\"\"");
    }

    #[test]
    fn js_string_html_script_close() {
        // </script> in a JS string inside HTML could break out of script tags
        let result = js_string("</script><img onerror=alert(1)>");
        assert!(result.starts_with('"'));
        // The string is JSON-encoded; verify it round-trips safely
        let decoded: String = serde_json::from_str(&result).unwrap();
        assert_eq!(decoded, "</script><img onerror=alert(1)>");
    }

    #[test]
    fn js_string_very_long() {
        let long = "a".repeat(100_000);
        let result = js_string(&long);
        assert!(result.len() >= 100_002); // quotes + content
    }

    // ── URL validation tests ────────────────────────────────────────────────

    #[test]
    fn url_allows_http() {
        assert!(validate_url("http://example.com", false).is_ok());
    }

    #[test]
    fn url_allows_https() {
        assert!(validate_url("https://example.com/path?q=1", false).is_ok());
    }

    #[test]
    fn url_allows_http_localhost() {
        assert!(validate_url("http://localhost:3000", false).is_ok());
    }

    #[test]
    fn url_blocks_file_by_default() {
        let err = validate_url("file:///etc/passwd", false).unwrap_err();
        assert!(err.contains("file"), "error should mention the file scheme");
    }

    #[test]
    fn url_allows_file_when_opted_in() {
        assert!(validate_url("file:///tmp/test.html", true).is_ok());
    }

    #[test]
    fn url_blocks_javascript() {
        assert!(validate_url("javascript:alert(1)", false).is_err());
    }

    #[test]
    fn url_blocks_javascript_case_insensitive() {
        assert!(validate_url("JAVASCRIPT:alert(1)", false).is_err());
    }

    #[test]
    fn url_blocks_data_scheme() {
        assert!(validate_url("data:text/html,<script>alert(1)</script>", false).is_err());
    }

    #[test]
    fn url_blocks_vbscript() {
        assert!(validate_url("vbscript:MsgBox(1)", false).is_err());
    }

    #[test]
    fn url_rejects_invalid() {
        assert!(validate_url("not a url at all", false).is_err());
    }

    #[test]
    fn url_strips_control_chars() {
        // Control characters should be stripped, leaving a valid URL
        let input = format!("http://example{}com", '\0');
        assert!(validate_url(&input, false).is_ok());
    }

    // ── CSS color sanitization tests ───────────────────────────────────────

    #[test]
    fn css_color_valid_hex() {
        assert_eq!(sanitize_css_color("#ff0000").unwrap(), "#ff0000");
        assert_eq!(sanitize_css_color("#FFF").unwrap(), "#FFF");
        assert_eq!(sanitize_css_color("#12345678").unwrap(), "#12345678");
    }

    #[test]
    fn css_color_valid_rgb() {
        assert_eq!(
            sanitize_css_color("rgb(255, 0, 0)").unwrap(),
            "rgb(255, 0, 0)"
        );
        assert_eq!(
            sanitize_css_color("rgba(0, 0, 0, 0.5)").unwrap(),
            "rgba(0, 0, 0, 0.5)"
        );
    }

    #[test]
    fn css_color_valid_named() {
        assert_eq!(sanitize_css_color("red").unwrap(), "red");
        assert_eq!(sanitize_css_color("transparent").unwrap(), "transparent");
    }

    #[test]
    fn css_color_valid_hsl() {
        assert_eq!(
            sanitize_css_color("hsl(120, 50%, 50%)").unwrap(),
            "hsl(120, 50%, 50%)"
        );
    }

    #[test]
    fn css_color_rejects_too_long() {
        let long = "a".repeat(101);
        assert!(sanitize_css_color(&long).is_err());
    }

    #[test]
    fn css_color_rejects_backslash_escapes() {
        assert!(sanitize_css_color(r"red\00").is_err());
        assert!(sanitize_css_color(r"\72\65\64").is_err());
    }

    #[test]
    fn css_color_rejects_url_injection() {
        assert!(sanitize_css_color("url(http://evil.com)").is_err());
        assert!(sanitize_css_color("URL(http://evil.com)").is_err());
    }

    #[test]
    fn css_color_rejects_expression_injection() {
        assert!(sanitize_css_color("expression(alert(1))").is_err());
        assert!(sanitize_css_color("EXPRESSION(alert(1))").is_err());
    }

    #[test]
    fn css_color_rejects_import() {
        assert!(sanitize_css_color("@import url(evil.css)").is_err());
    }

    #[test]
    fn css_color_rejects_semicolons_and_braces() {
        assert!(sanitize_css_color("red; background: url(evil)").is_err());
        assert!(sanitize_css_color("red} body { color: blue").is_err());
    }

    #[test]
    fn css_color_rejects_special_chars() {
        assert!(sanitize_css_color("red<script>").is_err());
        assert!(sanitize_css_color("red\"onload=alert").is_err());
        assert!(sanitize_css_color("red'onclick=alert").is_err());
    }

    #[test]
    fn css_color_trims_whitespace() {
        assert_eq!(sanitize_css_color("  red  ").unwrap(), "red");
    }

    #[test]
    fn css_color_empty_string() {
        assert_eq!(sanitize_css_color("").unwrap(), "");
    }
}
