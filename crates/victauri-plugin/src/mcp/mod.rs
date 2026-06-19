// This file is intentionally large (~3,400 lines). rmcp's `#[tool_router]`
// macro requires every `#[tool]` method to live in a single `impl` block, so
// splitting the handler across files would break tool registration. Parameter
// structs are already factored into sub-modules (webview_params, window_params,
// etc.) to keep this file focused on dispatch logic.

mod authz;
mod backend_params;
mod compound_params;
mod helpers;
mod introspection_params;
mod other_params;
mod rest;
mod server;
mod verification_params;
mod webview_params;
mod window_params;

use std::collections::{HashMap, HashSet};
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
    RecoveryHint, build_ghost_report, ghost_ipc_outcomes_js, ghost_ipc_projection_js,
    ipc_catalog_projection_js, ipc_timing_projection_js, ipc_timing_stats, js_string, json_result,
    json_truthy, merge_command_catalog, missing_param, sanitize_css_color, sanitize_injected_css,
    tool_disabled, tool_error, tool_error_with_hint, validate_url,
};

// MCP tool *parameter* types are an internal protocol surface: they are deserialized
// from MCP/JSON, used only inside this crate's (private) tool methods, and change every
// release as actions/fields are added. They are deliberately NOT part of the public API
// (`pub(crate)`, not `pub use`), so adding a tool action or field is not a breaking change
// and `cargo semver-checks` stays meaningful. Only `server::*` (build_app*,
// VictauriMcpHandler) is the public MCP surface consumers actually use.
pub(crate) use backend_params::*;
pub(crate) use compound_params::*;
pub(crate) use introspection_params::*;
pub(crate) use other_params::{
    AppStateParams, DiagnosticsParams, FindElementsParams, ResolveCommandParams,
    SemanticAssertParams, WaitCondition, WaitForParams,
};
pub use server::*;
pub(crate) use verification_params::*;
pub(crate) use webview_params::*;
pub(crate) use window_params::*;

// ── MCP Handler ──────────────────────────────────────────────────────────────

/// Maximum number of in-flight JavaScript eval requests. Prevents unbounded
/// growth of the `pending_evals` map if callbacks are never resolved.
pub(crate) const MAX_PENDING_EVALS: usize = 100;

fn chrono_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Maximum length of JavaScript code accepted by the `eval_js` tool (1 MB).
const MAX_EVAL_CODE_LEN: usize = 1_000_000;

/// Maximum length of a JavaScript eval return value (5 MB).
/// Results exceeding this are truncated to prevent memory exhaustion.
const MAX_EVAL_RESULT_LEN: usize = 5_000_000;

/// How long the eval parse-watchdog waits for the user-code script to begin executing
/// before reporting a likely syntax error. A parse error means the script never runs (so
/// it never marks itself "started"); this caps that failure at ~0.75s instead of the full
/// eval timeout, while still leaving valid-but-slow code to run to the real timeout.
const PARSE_WATCHDOG_MS: u64 = 750;

/// Default number of entries returned by IPC/network log tools when no explicit
/// `limit` is given. Prevents busy apps (large logs) from exceeding the eval cap.
const DEFAULT_LOG_LIMIT: usize = 100;

/// Per-field byte cap applied to each IPC/network log entry before serialization.
/// Large request/response bodies are truncated with a marker so the aggregate
/// log stays well under [`MAX_EVAL_RESULT_LEN`] even on heavy-traffic apps.
const MAX_LOG_FIELD_BYTES: usize = 4096;

/// Hard cap on entries returned by `list_app_dir` (recursive). Without it a
/// directory with millions of files (or a wide tree at max depth) would build an
/// unbounded result Vec and blow the eval/output cap (audit B7). When hit, the
/// listing stops and the response is marked `truncated: true`.
const MAX_DIR_ENTRIES: usize = 10_000;

/// `db_health` performs integrity checks and table counts against app-owned
/// databases. Bound the diagnostic so a large or adversarial DB cannot hold a
/// blocking worker indefinitely or return an unbounded schema listing.
#[cfg(feature = "sqlite")]
const DB_HEALTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(feature = "sqlite")]
const DB_HEALTH_PROGRESS_OPS: i32 = 10_000;
#[cfg(feature = "sqlite")]
const MAX_DB_HEALTH_TABLES: usize = 1_000;
#[cfg(feature = "sqlite")]
const MAX_DB_HEALTH_TABLE_BYTES: usize = 1_000_000;
#[cfg(feature = "sqlite")]
const MAX_DB_HEALTH_CELL_BYTES: i32 = 1_048_576;

const RESOURCE_URI_IPC_LOG: &str = "victauri://ipc-log";
const RESOURCE_URI_WINDOWS: &str = "victauri://windows";
const RESOURCE_URI_STATE: &str = "victauri://state";

/// Map an MCP resource URI to the privacy capability that gates its
/// tool-equivalent read. Resources are served outside the tool dispatcher, so
/// this lets `read_resource`/`subscribe` apply the same privacy matrix (audit
/// B1). Returns `None` for an unknown URI (handled as not-found downstream).
fn resource_required_capability(uri: &str) -> Option<&'static str> {
    match uri {
        // Reading the IPC log via a resource == the `logs ipc` tool action.
        RESOURCE_URI_IPC_LOG => Some("logs.ipc"),
        // Window states == the `window list` action.
        RESOURCE_URI_WINDOWS => Some("window.list"),
        // The state summary == reading plugin info.
        RESOURCE_URI_STATE => Some("get_plugin_info"),
        _ => None,
    }
}

const BRIDGE_VERSION: &str = env!("CARGO_PKG_VERSION");

const SAFE_ENV_PREFIXES: &[&str] = &[
    "HOME",
    "USER",
    "LANG",
    "LC_",
    "TERM",
    "SHELL",
    "DISPLAY",
    "XDG_",
    // Only Tauri's build-env namespace, NOT all of TAURI_ — the latter is an
    // app-custom namespace that can hold secrets (audit #5).
    "TAURI_ENV_",
    "VICTAURI_",
    "NODE_ENV",
    "OS",
    "HOSTNAME",
    "PWD",
    "SHLVL",
    "LOGNAME",
];

/// Substrings that mark an env var as a secret. Even when a name matches a
/// `SAFE_ENV_PREFIXES` entry it is dropped if it contains one of these — a prefix
/// like `TAURI_`/`VICTAURI_` otherwise leaks `TAURI_SIGNING_PRIVATE_KEY`,
/// `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, or `VICTAURI_AUTH_TOKEN` (audit #5).
const SECRET_ENV_SUBSTRINGS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASS", // PASSWORD, PASSWD, PASSPHRASE
    "PRIVATE",
    "CREDENTIAL",
    "APIKEY",
    "AUTH",
    "_KEY",
    "DSN", // connection strings with embedded creds
    "PAT", // personal access token
    "JWT",
    "BEARER",
    "SESSION",
    "COOKIE",
    "SALT",
    "CERT",
    "SIGN", // signing keys/material
    "LICENSE",
];

/// Whether an env var name is safe to surface via `app_info`: it must match a
/// known-safe prefix AND not look like a secret (audit #5).
fn is_safe_env_key(key: &str) -> bool {
    let upper = key.to_uppercase();
    SAFE_ENV_PREFIXES
        .iter()
        .any(|prefix| upper.starts_with(prefix))
        && !SECRET_ENV_SUBSTRINGS.iter().any(|s| upper.contains(s))
}

/// MCP tool handler that dispatches tool calls to the webview bridge and state.
#[derive(Clone)]
pub struct VictauriMcpHandler {
    state: Arc<VictauriState>,
    bridge: Arc<dyn WebviewBridge>,
    subscriptions: Arc<Mutex<HashSet<String>>>,
    bridge_checked: Arc<AtomicBool>,
    /// Window keys whose previous eval timed out. Retained only to annotate the
    /// error on the *next* eval (the bridge is probed before every eval anyway).
    timed_out_labels: Arc<Mutex<HashSet<String>>>,
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
        match self
            .eval_with_return(&code, params.webview_label.as_deref())
            .await
        {
            Ok(result) => {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&result)
                    && let Some(err) = parsed.get("error").and_then(|e| e.as_str())
                {
                    return tool_error(err);
                }
                CallToolResult::success(vec![Content::text(result)])
            }
            Err(e) => tool_error(e),
        }
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

        // ── Fault injection check ──
        if let Some(fault) = self.state.fault_registry.check_and_trigger(&params.command) {
            match fault {
                crate::introspection::FaultType::Delay { delay_ms } => {
                    tracing::info!(
                        command = %params.command,
                        delay_ms = delay_ms,
                        "fault injection: delaying command"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    // After delay, continue with normal execution below
                }
                crate::introspection::FaultType::Error { ref message } => {
                    tracing::info!(
                        command = %params.command,
                        "fault injection: returning error"
                    );
                    return tool_error(format!(
                        "[FAULT INJECTED] command '{}': {message}",
                        params.command
                    ));
                }
                crate::introspection::FaultType::Drop => {
                    tracing::info!(
                        command = %params.command,
                        "fault injection: dropping response"
                    );
                    return CallToolResult::success(vec![Content::text("{}")]);
                }
                crate::introspection::FaultType::Corrupt => {
                    tracing::info!(
                        command = %params.command,
                        "fault injection: corrupting response"
                    );
                    // Execute normally but mangle the response
                    let args_json = params.args.unwrap_or(serde_json::json!({}));
                    let args_str =
                        serde_json::to_string(&args_json).unwrap_or_else(|_| "{}".to_string());
                    let code = format!(
                        "return window.__TAURI_INTERNALS__.invoke({}, {args_str})",
                        js_string(&params.command)
                    );
                    if let Ok(result) = self
                        .eval_with_return(&code, params.webview_label.as_deref())
                        .await
                    {
                        let corrupted = format!(
                            "{{\"__corrupted\":true,\"original_length\":{},\"fault\":\"corrupt\"}}",
                            result.len()
                        );
                        return CallToolResult::success(vec![Content::text(corrupted)]);
                    }
                    return CallToolResult::success(vec![Content::text(
                        "{\"__corrupted\":true,\"fault\":\"corrupt\",\"note\":\"original invocation also failed\"}",
                    )]);
                }
            }
        }

        // ── Normal execution with timing ──
        let start = std::time::Instant::now();
        let args_json = params.args.unwrap_or(serde_json::json!({}));
        let args_str = serde_json::to_string(&args_json).unwrap_or_else(|_| "{}".to_string());
        let code = format!(
            "return window.__TAURI_INTERNALS__.invoke({}, {args_str})",
            js_string(&params.command)
        );
        let result = self
            .eval_with_return(&code, params.webview_label.as_deref())
            .await;
        let elapsed = start.elapsed();
        self.state.command_timings.record(&params.command, elapsed);

        match result {
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
        description = "Capture a screenshot of a Tauri window as a base64-encoded PNG image. Works on Windows (PrintWindow), macOS (CGWindowListCreateImage), and Linux X11/XWayland. Pure Wayland fails safely because its available fallback would capture the full desktop rather than the requested window. A hidden (non-visible) window has no on-screen surface to capture, so requesting one returns a clear error (show it first via `window` manage_action=show) rather than a stale or wrong-window image.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn screenshot(&self, Parameters(params): Parameters<ScreenshotParams>) -> CallToolResult {
        if !self.state.privacy.is_tool_enabled("screenshot") {
            return tool_disabled("screenshot");
        }
        // Resolve the EXACT window to capture and require it to be visible BEFORE touching the
        // OS handle. A native screenshot captures the on-screen surface; a hidden window has
        // none, so the OS capture path (PrintWindow / CGWindowListCreateImage) silently returns
        // stale, empty, or ANOTHER window's pixels with no error — an agent can't tell a wrong
        // image from a right one, so a silent wrong-window image is worse than a clear failure.
        // Two failure modes, both closed here:
        //   1. (live-4DA, 2026-06-16) an explicitly-requested hidden window (label:"briefing")
        //      returned the MAIN window's pixels.
        //   2. (GPT audit, P2) `screenshot {}` with no label: the default resolver
        //      `find_window(None)` prefers "main" UNCONDITIONALLY, so an app that hides main but
        //      leaves a secondary window visible captured hidden main. We do NOT change
        //      `find_window` — other callers (e.g. eval) may legitimately target a hidden window
        //      — instead the screenshot tool resolves its own VISIBLE target and passes it
        //      explicitly to `get_native_handle`.
        // Note (acknowledged residual): resolve -> handle -> capture are separate calls, so a
        // window hidden in the gap is still TOCTOU. The worst case is the same usability failure
        // (a wrong image), not a security boundary; the guard makes the common, deterministic
        // case correct.
        let states = self.bridge.get_window_states(None);
        let target_label: String = if let Some(label) = params.window_label.as_deref() {
            // An explicit label that the enumerator reports hidden is rejected; visible or
            // unknown labels fall through (an unknown one lets get_native_handle produce the
            // canonical "window not found" rather than inventing an error here).
            if states.iter().any(|s| s.label == label && !s.visible) {
                return tool_error(format!(
                    "window '{label}' is not visible — a native screenshot captures the \
                     on-screen surface, and a hidden window has none (the OS capture would \
                     return stale or another window's pixels). Show it first \
                     (window action=manage manage_action=show label={label}), then capture."
                ));
            }
            label.to_string()
        } else {
            // No label: prefer a visible "main", else the first visible window. NEVER silently
            // fall back to a hidden window (the P2 bug) — error instead.
            let pick = states
                .iter()
                .find(|s| s.label == "main" && s.visible)
                .or_else(|| states.iter().find(|s| s.visible));
            match pick {
                Some(st) => st.label.clone(),
                None => {
                    return tool_error(
                        "no visible window to capture — every window is hidden (or the UI is \
                         not responding). Show one first (window action=manage \
                         manage_action=show label=<label>), then capture."
                            .to_string(),
                    );
                }
            }
        };
        match self.bridge.get_native_handle(Some(&target_label)) {
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
            // Gate on BOTH is_invoke_allowed and is_command_allowed, matching
            // invoke_command and the contract/replay paths — backend_command
            // previously checked only the blocklist (audit #30 follow-up).
            if !self.state.privacy.is_invoke_allowed(cmd)
                || !self.state.privacy.is_command_allowed(cmd)
            {
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
        description = "Detect ghost commands (frontend calls with no backend handler) by IPC OUTCOME, not by guessing from Victauri's registry. Returns: `confirmed_ghosts` = commands invoked that NEVER returned success and errored 'not found' — real missing-handler bugs, HIGH confidence and independent of whether the app uses #[inspectable]; `verified_handlers` = count of commands that returned success at least once (they provably HAVE a handler, so they are never flagged — this is why a real command like `set_language` is no longer a false positive); `frontend_only` = the WEAKER candidate tier (invoked, never observed succeeding, NOT a Tauri/plugin framework builtin, and absent from the introspection registry) — confirm against the app's `tauri::generate_handler!` before filing; `excluded_builtins` = framework `plugin:*` commands (never app ghosts); `registry_only` = registered commands never invoked (informational). The `reliability` field describes only `frontend_only`; `confirmed_ghosts` is high-confidence regardless. Reads the JS-side IPC interception log (ACCUMULATES all session traffic). For a clean signal scope with `since_ms` (e.g. 5000) — invoke the suspect action, then call this with `since_ms` — or `logs {action:'clear'}` then exercise the app.",
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
        // Project a per-command OUTCOME summary in JS ({command, ok, err}, deduped). Ghost
        // detection is outcome-based (VIC-1): a command that returned success provably has a
        // handler and is never a ghost; one that errored "not found" is a confirmed ghost.
        // Aggregating per command keeps this tiny even on a busy app (avoids the eval cap).
        // When `since_ms` is set, the projection time-windows to the current test's traffic.
        let code = ghost_ipc_outcomes_js(params.since_ms);
        let ipc_json = match self
            .eval_with_return(&code, params.webview_label.as_deref())
            .await
        {
            Ok(r) => r,
            Err(e) => return tool_error(format!("failed to read IPC log: {e}")),
        };

        let outcomes: Vec<crate::mcp::helpers::IpcOutcome> = match serde_json::from_str(&ipc_json) {
            Ok(v) => v,
            Err(e) => return tool_error(format!("failed to parse IPC log JSON: {e}")),
        };

        json_result(&build_ghost_report(&outcomes, &self.state.registry))
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
                var net = window.__VICTAURI__?.getNetworkLog() || [];
                var warning = null;
                if (log.length === 0 && net.length > 5) {{
                    warning = 'Zero IPC calls captured but ' + net.length + ' network requests observed. IPC capture may not be working — verify the app uses Tauri IPC via fetch to ipc.localhost.';
                }}
                // INTEGRITY = round-trip soundness: no stuck/stale (never-returned) calls.
                // A command that completed with an Err is a HEALTHY round-trip (it returned)
                // — every real app exercises error paths, so counting those as 'unhealthy'
                // would cry wolf. The error_count/errored_calls surface them for visibility,
                // but only stale calls flip `healthy`.
                return {{
                    healthy: stale.length === 0,
                    total_calls: log.length,
                    pending_count: pending.length,
                    stale_count: stale.length,
                    error_count: errored.length,
                    stale_calls: stale.slice(0, 20),
                    errored_calls: errored.slice(0, 20),
                    warning: warning
                }};
            }})()"
        );
        self.eval_bridge(&code, params.webview_label.as_deref())
            .await
    }

    #[tool(
        description = "Wait for a condition to be met. Polls at regular intervals until satisfied or timeout. Conditions: text (text appears), text_gone (text disappears), selector (CSS selector matches), selector_gone, url (URL contains value), ipc_idle (no pending IPC calls), network_idle (no pending network requests), expression (poll a JS expression in `value` until truthy or until it equals `expected` — may `await`, e.g. await a fire-and-forget command's status), event (block until the Tauri event named in `value` fires, with `since_ms` look-back). Use expression/event to await async backend work to true completion instead of guessing with a fixed sleep.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn wait_for(&self, Parameters(params): Parameters<WaitForParams>) -> CallToolResult {
        let timeout_ms = params.timeout_ms.unwrap_or(10_000).min(120_000);
        let poll = params.poll_ms.unwrap_or(200).max(20);

        // The `expression` and `event` conditions are awaited server-side (they
        // poll the eval engine and the captured event bus respectively), so a
        // fire-and-forget backend command can be awaited to true completion.
        match params.condition {
            WaitCondition::Expression => {
                return self.wait_for_expression(&params, timeout_ms, poll).await;
            }
            WaitCondition::Event => {
                return self.wait_for_event(&params, timeout_ms, poll).await;
            }
            _ => {}
        }

        let value = params
            .value
            .as_ref()
            .map_or_else(|| "null".to_string(), |v| js_string(v));
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

    /// Poll a JS expression until truthy (or `== expected`), server-side.
    ///
    /// Level-triggered and race-free: each poll re-evaluates the expression via
    /// the same engine as `eval_js`, so it may `await`. Eval errors are treated
    /// as "not yet met" (the target may not exist during startup) and the last
    /// error is surfaced on timeout.
    async fn wait_for_expression(
        &self,
        params: &WaitForParams,
        timeout_ms: u64,
        poll_ms: u64,
    ) -> CallToolResult {
        if !self.state.privacy.is_tool_enabled("eval_js") {
            return tool_disabled("wait_for(expression) requires eval_js capability");
        }
        let Some(expr) = params.value.as_deref().filter(|s| !s.is_empty()) else {
            return missing_param("value", "wait_for(expression)");
        };
        let code = format!("return ({expr});");
        let start = std::time::Instant::now();
        let deadline = start + std::time::Duration::from_millis(timeout_ms);
        let poll = std::time::Duration::from_millis(poll_ms);
        let mut last_value = serde_json::Value::Null;
        let mut last_error: Option<String> = None;

        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let per_eval = remaining
                .min(std::time::Duration::from_secs(15))
                .max(std::time::Duration::from_secs(1));
            match self
                .eval_with_return_timeout(&code, params.webview_label.as_deref(), per_eval)
                .await
            {
                Ok(raw) => {
                    let val = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
                    let met = match &params.expected {
                        Some(expected) => &val == expected,
                        None => json_truthy(&val),
                    };
                    if met {
                        return json_result(&serde_json::json!({
                            "ok": true,
                            "value": val,
                            "elapsed_ms": start.elapsed().as_millis() as u64,
                        }));
                    }
                    last_value = val;
                }
                Err(e) => last_error = Some(e),
            }

            if std::time::Instant::now() >= deadline {
                return json_result(&serde_json::json!({
                    "ok": false,
                    "error": format!("timeout after {timeout_ms}ms"),
                    "last_value": last_value,
                    "last_error": last_error,
                    "elapsed_ms": start.elapsed().as_millis() as u64,
                }));
            }
            tokio::time::sleep(
                poll.min(deadline.saturating_duration_since(std::time::Instant::now())),
            )
            .await;
        }
    }

    /// Block until a named Tauri event appears on the captured event bus.
    ///
    /// Edge-triggered: matches the most recent event whose timestamp is no older
    /// than `since_ms` before this call began, so an event fired in the gap
    /// between `invoke_command` and this call is still caught. Polls the
    /// event-bus ring buffer — no webview eval involved.
    async fn wait_for_event(
        &self,
        params: &WaitForParams,
        timeout_ms: u64,
        poll_ms: u64,
    ) -> CallToolResult {
        let Some(name) = params.value.as_deref().filter(|s| !s.is_empty()) else {
            return missing_param("value", "wait_for(event)");
        };
        let since_ms = params.since_ms.unwrap_or(2000);
        let start = std::time::Instant::now();
        let baseline = chrono::Utc::now()
            - chrono::TimeDelta::try_milliseconds(since_ms as i64).unwrap_or_default();
        let deadline = start + std::time::Duration::from_millis(timeout_ms);
        let poll = std::time::Duration::from_millis(poll_ms);

        loop {
            // Search newest-first for a matching event no older than the baseline.
            let matched = self.state.event_bus.events().into_iter().rev().find(|e| {
                e.name == name
                    && chrono::DateTime::parse_from_rfc3339(&e.timestamp)
                        .map_or(true, |ts| ts.with_timezone(&chrono::Utc) >= baseline)
            });
            if let Some(ev) = matched {
                return json_result(&serde_json::json!({
                    "ok": true,
                    "event": {
                        "name": ev.name,
                        "payload": ev.payload,
                        "timestamp": ev.timestamp,
                    },
                    "elapsed_ms": start.elapsed().as_millis() as u64,
                }));
            }
            if std::time::Instant::now() >= deadline {
                return json_result(&serde_json::json!({
                    "ok": false,
                    "error": format!("timeout after {timeout_ms}ms waiting for event '{name}'"),
                    "hint": "Ensure the app emits this Tauri event and Victauri captures it: \
                             custom events need VictauriBuilder::listen_events(&[\"…\"]); \
                             window-lifecycle events are captured automatically.",
                    "elapsed_ms": start.elapsed().as_millis() as u64,
                }));
            }
            tokio::time::sleep(poll).await;
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
        let limit = params.limit.unwrap_or(5);
        let mut results = self.state.registry.resolve(&params.query);
        results.truncate(limit);
        json_result(&results)
    }

    #[tool(
        description = "List or search all registered Tauri commands with their argument schemas. Pass query to filter by name/description substring. Commands are registered via the #[inspectable] macro — apps that don't use it return names with null schemas; for those, use `introspect command_catalog` to recover real argument/result shapes from the live IPC log.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_registry(&self, Parameters(params): Parameters<RegistryParams>) -> CallToolResult {
        let commands = match params.query {
            Some(q) => self.state.registry.search(&q),
            None => self.state.registry.list(),
        };
        json_result(&commands)
    }

    #[tool(
        description = "Read application-defined backend state via a registered probe. With no `probe`, lists available probe names. With a `probe` name, runs it and returns its JSON snapshot. Probes give first-class, discoverable access to domain state (e.g. a scoring pipeline's version + stale-item count, a queue's depth, cache stats) that would otherwise need query_db + log-grepping. Probes run in the Rust process with no IPC round-trip. Apps register them via VictauriBuilder::probe(name, closure).",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn app_state(&self, Parameters(params): Parameters<AppStateParams>) -> CallToolResult {
        let Some(name) = params.probe else {
            return json_result(&serde_json::json!({ "probes": self.state.probes.names() }));
        };
        if let Some(value) = self.state.probes.run(&name) {
            json_result(&value)
        } else {
            let available = self.state.probes.names();
            tool_error_with_hint(
                format!(
                    "unknown probe '{name}'. Available probes: {}",
                    if available.is_empty() {
                        "(none registered — add VictauriBuilder::probe(\"name\", ...))".to_string()
                    } else {
                        available.join(", ")
                    }
                ),
                RecoveryHint::CheckInput,
            )
        }
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

        // Host-app identity: lets an agent verify on its FIRST call that it reached the
        // intended app (not another Victauri instance sharing the discovery port).
        let app_cfg = self.bridge.tauri_config();
        let result = serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "bridge_version": BRIDGE_VERSION,
            "port": self.state.port.load(Ordering::Relaxed),
            "app": {
                "identifier": app_cfg.get("identifier"),
                "product_name": app_cfg.get("product_name"),
            },
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
        let config = self.bridge.tauri_config();

        let data_dir = self.bridge.app_data_dir().ok();
        let config_dir = self.bridge.app_config_dir().ok();
        let log_dir = self.bridge.app_log_dir().ok();
        let local_data_dir = self.bridge.app_local_data_dir().ok();

        let env_vars: std::collections::BTreeMap<String, String> = std::env::vars()
            .filter(|(k, _)| is_safe_env_key(k))
            .collect();

        // Enumerate every database candidate across ALL roots (configured db_search_paths
        // + every OS app dir), each tagged with size, whether it is a WebView/engine
        // internal store, and whether it is the one `query_db` would auto-select. This lets
        // an agent see and disambiguate the real app DB instead of guessing (audit /
        // red-team "wrong DB" finding — `app_info.databases` previously only walked
        // data_dir and returned bare relative names).
        #[cfg(feature = "sqlite")]
        let databases: Vec<serde_json::Value> = {
            let mut all_dirs: Vec<std::path::PathBuf> = self.state.db_search_paths.clone();
            for d in [
                data_dir.as_ref(),
                config_dir.as_ref(),
                log_dir.as_ref(),
                local_data_dir.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                all_dirs.push(d.clone());
            }
            let select_dirs: Vec<std::path::PathBuf> = if self.state.db_search_paths.is_empty() {
                all_dirs.clone()
            } else {
                self.state.db_search_paths.clone()
            };
            let selected = crate::database::select_app_database(&select_dirs).ok();
            crate::database::classify_databases(&all_dirs)
                .into_iter()
                .map(|c| {
                    serde_json::json!({
                        "path": c.path.to_string_lossy(),
                        "size_bytes": c.size_bytes,
                        "webview_internal": c.webview_internal,
                        "selected": selected.as_ref() == Some(&c.path),
                    })
                })
                .collect()
        };

        #[cfg(not(feature = "sqlite"))]
        let databases: Vec<serde_json::Value> = Vec::new();

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
        let base = match self.resolve_app_dir(params.directory) {
            Ok(d) => d,
            Err(e) => return tool_error(e),
        };

        let target = if let Some(ref sub) = params.path {
            // Lexical traversal guard BEFORE the existence check: `safe_within`
            // canonicalizes (which errors on non-existent paths), so a `..` or
            // absolute sub-path must be rejected as traversal up front rather
            // than falling through to a misleading "does not exist" result.
            if let Err(e) = Self::lexical_safe(std::path::Path::new(sub)) {
                return tool_error(e);
            }
            let resolved = base.join(sub);
            // A missing directory is a normal, non-error result.
            if !resolved.exists() {
                return json_result(&serde_json::json!({
                    "base": base.to_string_lossy(),
                    "path": sub,
                    "exists": false,
                    "entries": [],
                    "count": 0,
                }));
            }
            if let Err(e) = Self::safe_within(&base, &resolved) {
                return tool_error(e);
            }
            resolved
        } else {
            base.clone()
        };

        // A missing base directory is a normal, non-error result.
        if !target.exists() {
            return json_result(&serde_json::json!({
                "base": base.to_string_lossy(),
                "path": params.path.unwrap_or_default(),
                "exists": false,
                "entries": [],
                "count": 0,
            }));
        }

        let max_depth = params.max_depth.unwrap_or(1).min(5);
        let pattern = params.pattern.as_deref();
        let mut entries = Vec::new();

        Self::list_dir_recursive(&target, &base, 0, max_depth, pattern, &mut entries);
        let truncated = entries.len() >= MAX_DIR_ENTRIES;

        json_result(&serde_json::json!({
            "base": base.to_string_lossy(),
            "path": params.path.unwrap_or_default(),
            "exists": true,
            "entries": entries,
            "count": entries.len(),
            "truncated": truncated,
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
        let base = match self.resolve_app_dir(params.directory) {
            Ok(d) => d,
            Err(e) => return tool_error(e),
        };

        // Lexical traversal guard FIRST — before the existence check — so a
        // traversal attempt (`..` / absolute) is rejected as traversal rather
        // than leaking whether the out-of-tree target exists via "file not
        // found". `safe_within` (which canonicalizes) stays below as
        // defense-in-depth for real files.
        if let Err(e) = Self::lexical_safe(std::path::Path::new(&params.path)) {
            return tool_error(e);
        }
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

        // Open the CANONICAL validated path, and do the blocking file IO on the blocking pool.
        // Doing sync `std::fs` IO directly in this async fn could stall the executor thread if a
        // regular file were swapped (between the `safe_within` check and the open) for a FIFO or
        // slow device whose `read_to_end` blocks; opening the canonical path also closes the
        // trivial validate-lexical / open-lexical symlink-swap window.
        let canonical = match std::fs::canonicalize(&target) {
            Ok(c) => c,
            Err(e) => return tool_error(format!("cannot resolve path: {e}")),
        };
        #[allow(clippy::cast_possible_truncation)]
        let read = tokio::task::spawn_blocking(
            move || -> Result<(Vec<u8>, usize, Option<u64>), String> {
                use std::io::Read;
                let metadata = std::fs::metadata(&canonical).ok();
                let size = metadata.as_ref().map(|m| m.len() as usize);
                let modified = metadata.as_ref().and_then(|m| m.modified().ok()).map(|t| {
                    t.duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                });
                // Bounded read (audit B7): pull at most max_bytes+1 instead of slurping the
                // whole file. The +1 detects truncation; the reported size comes from metadata.
                let f = std::fs::File::open(&canonical).map_err(|e| e.to_string())?;
                let mut buf = Vec::new();
                f.take(max_bytes as u64 + 1)
                    .read_to_end(&mut buf)
                    .map_err(|e| e.to_string())?;
                let reported = size.unwrap_or(buf.len());
                Ok((buf, reported, modified))
            },
        )
        .await;
        let (mut bytes, original_size, modified) = match read {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return tool_error(format!("failed to read file: {e}")),
            Err(e) => return tool_error(format!("file read task failed: {e}")),
        };
        let truncated = bytes.len() > max_bytes;
        if truncated {
            bytes.truncate(max_bytes);
        }

        let file_info = serde_json::json!({
            "path": params.path,
            "size": original_size,
            "truncated": truncated,
            "modified": modified,
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

    #[tool(
        description = "Execute a bounded, read-only SQL query against a SQLite database in the app's data directory. The SQL goes in the `query` field (alias: `sql`). Auto-discovers database files if no path is specified. Only SELECT/PRAGMA/EXPLAIN/WITH queries are allowed. CPU time, cell size, row count, and returned bytes are capped. Returns rows as JSON objects with column names as keys. This provides direct backend database access without going through the webview or IPC.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn query_db(&self, Parameters(params): Parameters<QueryDbParams>) -> CallToolResult {
        // query_db is ALWAYS registered as a tool so the rmcp `#[tool_router]` macro
        // compiles with `default-features = false` (a consumer that drops the heavy
        // rusqlite C dependency). The actual SQLite implementation only exists with the
        // `sqlite` feature; without it, return a clear, actionable error.
        #[cfg(feature = "sqlite")]
        {
            self.query_db_impl(params).await
        }
        #[cfg(not(feature = "sqlite"))]
        {
            let _ = params;
            tool_error(
                "query_db is unavailable: this build was compiled without the 'sqlite' \
                 feature (default-features = false). Re-enable the 'sqlite' feature to use it.",
            )
        }
    }

    /// Real `query_db` implementation — compiled only with the `sqlite` feature.
    #[cfg(feature = "sqlite")]
    async fn query_db_impl(&self, params: QueryDbParams) -> CallToolResult {
        let data_dir = match self.bridge.app_data_dir() {
            Ok(d) => d,
            Err(e) => return tool_error(format!("cannot access app data directory: {e}")),
        };

        let app_dirs: Vec<std::path::PathBuf> = [
            self.bridge.app_data_dir(),
            self.bridge.app_config_dir(),
            self.bridge.app_local_data_dir(),
            self.bridge.app_log_dir(),
        ]
        .into_iter()
        .filter_map(Result::ok)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
        // Explicitly-configured roots (VictauriBuilder::db_search_paths) take
        // precedence over OS app directories for auto-discovery, so a configured
        // application DB wins over incidental ones (e.g. WebView internals).
        let mut search_dirs: Vec<std::path::PathBuf> = self.state.db_search_paths.clone();
        search_dirs.extend(app_dirs);

        let db_path = if let Some(ref requested_path) = params.path {
            match Self::resolve_existing_db_path(&search_dirs, requested_path) {
                Ok(path) => path,
                Err(e) => return tool_error(e),
            }
        } else {
            // Auto-select the application DB. When db_search_paths is configured it is
            // EXCLUSIVE — never fall back to OS app dirs (which hold WebView internals),
            // so a configured-but-empty root yields a clear error instead of silently
            // querying the wrong database. WebView/browser-engine internal stores are
            // excluded and the largest remaining candidate wins (audit / red-team "wrong
            // DB" finding).
            let select_dirs: Vec<std::path::PathBuf> = if self.state.db_search_paths.is_empty() {
                search_dirs.clone()
            } else {
                self.state.db_search_paths.clone()
            };
            match crate::database::select_app_database(&select_dirs) {
                Ok(p) => p,
                Err(e) => return tool_error(e),
            }
        };

        let db_display = db_path
            .strip_prefix(&data_dir)
            .unwrap_or(&db_path)
            .to_string_lossy()
            .into_owned();
        let bind_params = params.params.unwrap_or_default();
        let query = params.query;
        let max_rows = params.max_rows;

        match tokio::task::spawn_blocking(move || {
            crate::database::query(&db_path, &query, &bind_params, max_rows)
        })
        .await
        {
            Ok(Ok(mut result)) => {
                if let Some(obj) = result.as_object_mut() {
                    obj.insert("database".to_string(), serde_json::json!(db_display));
                }
                json_result(&result)
            }
            Ok(Err(e)) => tool_error(e),
            Err(e) => tool_error(format!("database query task failed: {e}")),
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
                if params.trusted.unwrap_or(false) {
                    // Resolve the element's viewport-center coords, run the
                    // actionability check, then deliver a real OS click.
                    let probe = format!(
                        "var __e=window.__VICTAURI__&&window.__VICTAURI__.getRef({}); \
                         if(!__e) return null; __e.scrollIntoView({{block:'center',inline:'center',behavior:'instant'}}); \
                         var __b=__e.getBoundingClientRect(); \
                         return {{x:__b.left+__b.width/2, y:__b.top+__b.height/2}}",
                        js_string(ref_id)
                    );
                    let raw = match self
                        .eval_with_return(&probe, params.webview_label.as_deref())
                        .await
                    {
                        Ok(r) => r,
                        Err(e) => return tool_error(e),
                    };
                    let Ok(point) = serde_json::from_str::<serde_json::Value>(&raw) else {
                        return tool_error_with_hint(
                            format!("ref not found: {ref_id}"),
                            RecoveryHint::CheckInput,
                        );
                    };
                    let (Some(x), Some(y)) = (
                        point.get("x").and_then(serde_json::Value::as_f64),
                        point.get("y").and_then(serde_json::Value::as_f64),
                    ) else {
                        return tool_error_with_hint(
                            format!("ref not found: {ref_id}"),
                            RecoveryHint::CheckInput,
                        );
                    };
                    let bridge = self.bridge.clone();
                    let label = params.webview_label.clone();
                    let native = tokio::task::spawn_blocking(move || {
                        bridge.native_click(label.as_deref(), x, y)
                    })
                    .await
                    .unwrap_or_else(|e| Err(format!("native input task failed: {e}")));
                    return match native {
                        Ok(()) => json_result(
                            &serde_json::json!({"ok": true, "trusted": true, "x": x, "y": y}),
                        ),
                        Err(e) => tool_error(e),
                    };
                }
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
                let values_vec;
                let values: &[String] = match (&params.values, &params.value) {
                    (Some(v), _) => v,
                    (None, Some(v)) => {
                        values_vec = vec![v.clone()];
                        &values_vec
                    }
                    (None, None) => &[],
                };
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
                if params.trusted.unwrap_or(false) {
                    // Focus the element via JS, then deliver real OS keystrokes
                    // (isTrusted: true) for handlers that reject synthetic events.
                    let focus = format!(
                        "var __e=window.__VICTAURI__&&window.__VICTAURI__.getRef({}); if(__e){{__e.focus();}} return !!__e",
                        js_string(ref_id)
                    );
                    let focused = self
                        .eval_with_return(&focus, params.webview_label.as_deref())
                        .await
                        .unwrap_or_default();
                    if focused != "true" {
                        return tool_error_with_hint(
                            format!("ref not found or not focusable: {ref_id}"),
                            RecoveryHint::CheckInput,
                        );
                    }
                    let bridge = self.bridge.clone();
                    let label = params.webview_label.clone();
                    let text = text.to_string();
                    let native = tokio::task::spawn_blocking(move || {
                        bridge.native_type_text(label.as_deref(), &text)
                    })
                    .await
                    .unwrap_or_else(|e| Err(format!("native input task failed: {e}")));
                    return match native {
                        Ok(()) => json_result(&serde_json::json!({"ok": true, "trusted": true})),
                        Err(e) => tool_error(e),
                    };
                }
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
                if params.trusted.unwrap_or(false) {
                    // Optionally focus a target element, then send a real OS key.
                    if let Some(ref_id) = &params.ref_id {
                        let focus = format!(
                            "var __e=window.__VICTAURI__&&window.__VICTAURI__.getRef({}); if(__e){{__e.focus();}} return !!__e",
                            js_string(ref_id)
                        );
                        let _ = self
                            .eval_with_return(&focus, params.webview_label.as_deref())
                            .await;
                    }
                    let bridge = self.bridge.clone();
                    let label = params.webview_label.clone();
                    let key = key.to_string();
                    let native = tokio::task::spawn_blocking(move || {
                        bridge.native_key(label.as_deref(), &key)
                    })
                    .await
                    .unwrap_or_else(|e| Err(format!("native input task failed: {e}")));
                    return match native {
                        Ok(()) => json_result(&serde_json::json!({"ok": true, "trusted": true})),
                        Err(e) => tool_error(e),
                    };
                }
                let code = format!("return window.__VICTAURI__?.pressKey({})", js_string(key));
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
        }
    }

    #[tool(
        description = "Window management. Actions: get_state (window positions/sizes/visibility), list (all window labels), manage (minimize/maximize/close/focus/show/hide/fullscreen/always_on_top), resize, move_to, set_title, introspectability (probe every window and report which Victauri can actually see — a visible window that comes back introspectable:false is almost always missing the \"victauri:default\" capability; run this FIRST when eval_js/dom_snapshot/animation return nothing for a multi-window app).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn window(&self, Parameters(params): Parameters<WindowParams>) -> CallToolResult {
        match params.action {
            WindowAction::GetState => {
                let states = self.bridge.get_window_states(params.label.as_deref());
                // A specific label that matches no window is an error, not an
                // empty array (which reads as "success, no state").
                if states.is_empty()
                    && let Some(label) = params.label.as_deref()
                {
                    return tool_error(format!(
                        "window not found: '{label}' (use window.list to see available labels)"
                    ));
                }
                json_result(&states)
            }
            WindowAction::List => {
                let labels = self.bridge.list_window_labels();
                json_result(&labels)
            }
            WindowAction::Introspectability => self.window_introspectability().await,
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
                if width == 0 || height == 0 {
                    return tool_error_with_hint(
                        format!(
                            "invalid window size {width}x{height}: width and height must be > 0"
                        ),
                        RecoveryHint::CheckInput,
                    );
                }
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
                // Operator-protected keys (auth/role/tier/flags) can't be poisoned
                // via storage.set (audit #33).
                if !self.state.privacy.is_storage_key_allowed(key) {
                    return tool_error(format!(
                        "storage key '{key}' is protected by privacy configuration"
                    ));
                }
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
        description = "Time-travel recording. Actions: start (begin recording), stop (end and return session), checkpoint (save state snapshot), list_checkpoints, get_events (since index), events_between (two checkpoints), get_replay (IPC replay sequence), export (session as JSON), import (load session from JSON), replay (re-execute recorded IPC commands and compare responses), flush (immediately drain pending events into recording without waiting for the 1-second poll).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn recording(&self, Parameters(params): Parameters<RecordingParams>) -> CallToolResult {
        const MAX_SESSION_JSON: usize = 10 * 1024 * 1024;
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
                // checkpoint_id is optional — auto-generate a short id when the
                // caller just wants a positional marker. The id is echoed back in
                // the response so it can be referenced later
                // (events_between_checkpoints / replay).
                let id = params
                    .checkpoint_id
                    .unwrap_or_else(|| format!("cp-{}", uuid::Uuid::new_v4()));
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
            RecordingAction::Flush => {
                if !self.state.recorder.is_recording() {
                    return tool_error("no active recording — start a recording first");
                }
                let code = "return window.__VICTAURI__?.getEventStream(0)";
                match self
                    .eval_with_return(code, params.webview_label.as_deref())
                    .await
                {
                    Ok(result_str) => {
                        let events: Vec<serde_json::Value> =
                            serde_json::from_str(&result_str).unwrap_or_default();
                        let mut count = 0u64;
                        for ev in &events {
                            if let Some(app_event) = crate::mcp::server::parse_bridge_event(ev) {
                                self.state.event_log.push(app_event.clone());
                                self.state.recorder.record_event(app_event);
                                count += 1;
                            }
                        }
                        json_result(&serde_json::json!({
                            "flushed": true,
                            "events_captured": count,
                        }))
                    }
                    Err(e) => tool_error(format!("flush failed: {e}")),
                }
            }
            RecordingAction::Replay => {
                let calls = self.state.recorder.ipc_replay_sequence();
                if calls.is_empty() {
                    return tool_error("no IPC calls recorded — record a session first");
                }
                let mut replay_results = Vec::new();
                for call in &calls {
                    // Enforce the same command allow/blocklist as invoke_command
                    // (audit #30/#31): a recorded/imported session must not be able to
                    // invoke a command an operator blocked.
                    if !self.state.privacy.is_invoke_allowed(&call.command)
                        || !self.state.privacy.is_command_allowed(&call.command)
                    {
                        replay_results.push(serde_json::json!({
                            "command": call.command,
                            "status": "blocked",
                            "error": "blocked by privacy configuration",
                        }));
                        continue;
                    }
                    let code = format!(
                        "return window.__TAURI_INTERNALS__.invoke({})",
                        js_string(&call.command)
                    );
                    let outcome = match self
                        .eval_with_return(&code, params.webview_label.as_deref())
                        .await
                    {
                        Ok(result_str) => {
                            let value: serde_json::Value = serde_json::from_str(&result_str)
                                .unwrap_or(serde_json::Value::String(result_str));
                            let shape = crate::introspection::JsonShape::from_value(&value);
                            serde_json::json!({
                                "command": call.command,
                                "status": "ok",
                                "response_type": shape.type_name(),
                            })
                        }
                        Err(e) => {
                            serde_json::json!({
                                "command": call.command,
                                "status": "error",
                                "error": e,
                            })
                        }
                    };
                    replay_results.push(outcome);
                }
                let passed = replay_results
                    .iter()
                    .filter(|r| r.get("status").and_then(|s| s.as_str()) == Some("ok"))
                    .count();
                let result = serde_json::json!({
                    "replayed": replay_results.len(),
                    "passed": passed,
                    "failed": replay_results.len() - passed,
                    "results": replay_results,
                });
                json_result(&result)
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
                // highlight injects a debug overlay node into the page — a DOM
                // mutation — so it is gated separately and excluded from the
                // read-only Observe profile (red-team P1).
                if !self.state.privacy.is_tool_enabled("inspect.highlight") {
                    return tool_disabled("inspect.highlight");
                }
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
                if !self
                    .state
                    .privacy
                    .is_tool_enabled("inspect.clear_highlights")
                {
                    return tool_disabled("inspect.clear_highlights");
                }
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
                // Block remote @import / url(...) exfil vectors unless explicitly opted in.
                if let Err(e) = sanitize_injected_css(css, params.allow_remote) {
                    return tool_error(e);
                }
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
        description = "Network request interception (Playwright route() equivalent, no CDP). \
            Matches webview fetch/XHR by URL and blocks, mocks, or delays them. \
            Actions:\n\
            - `add`: add a rule. `pattern` (+ optional `match_type`: substring/glob/regex/exact, \
              and `method`) selects requests; `behavior` is `block` (abort), `fulfill` (return a \
              mock `status`/`headers`/`body`/`content_type`), or `delay` (proceed after `delay_ms`). \
              `times` limits how often it fires. Rules are page-scoped (cleared on reload).\n\
            - `list`: list active rules.\n\
            - `clear` (by `id`) / `clear_all`: remove rules.\n\
            - `matches`: log of intercepted requests.\n\
            Note: fetch supports all behaviors; XHR supports block/delay (fulfill is fetch-only). \
            Top-level navigation, sub-resource (img/css), and WebSocket traffic are not intercepted. \
            Tauri IPC (ipc.localhost) is OBSERVE-ONLY: such calls appear in `matches`, but block/\
            fulfill/delay do NOT take effect on them — Tauri serves IPC below the JS fetch layer, so \
            it cannot be controlled cross-platform without CDP. There is no IPC-control tool; the \
            `fault` tool only affects commands you drive via `invoke_command`, not real user IPC.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn route(&self, Parameters(params): Parameters<RouteParams>) -> CallToolResult {
        match params.action {
            RouteAction::Add => {
                if !self.state.privacy.is_tool_enabled("route.add") {
                    return tool_disabled("route.add");
                }
                let Some(pattern) = &params.pattern else {
                    return missing_param("pattern", "add");
                };
                let behavior = params.behavior.unwrap_or(RouteBehavior::Fulfill);
                let match_type = params.match_type.unwrap_or(RouteMatchType::Substring);
                let mut rule = serde_json::json!({
                    "pattern": pattern,
                    "match_type": match_type.as_str(),
                    "action": behavior.as_str(),
                });
                if let Some(m) = &params.method {
                    rule["method"] = serde_json::json!(m);
                }
                if let Some(s) = params.status {
                    rule["status"] = serde_json::json!(s);
                }
                if let Some(st) = &params.status_text {
                    rule["status_text"] = serde_json::json!(st);
                }
                if let Some(h) = &params.headers {
                    rule["headers"] = h.clone();
                }
                if let Some(b) = &params.body {
                    // A JSON string body is passed through as-is; structured JSON
                    // is stringified so the bridge sends valid JSON text.
                    rule["body"] = match b {
                        serde_json::Value::String(s) => serde_json::json!(s),
                        other => serde_json::json!(other.to_string()),
                    };
                }
                if let Some(ct) = &params.content_type {
                    rule["content_type"] = serde_json::json!(ct);
                }
                if let Some(d) = params.delay_ms {
                    rule["delay_ms"] = serde_json::json!(d);
                }
                if let Some(t) = params.times {
                    rule["times"] = serde_json::json!(t);
                }
                let code = format!(
                    "return window.__VICTAURI__?.addRoute({})",
                    js_string(&rule.to_string())
                );
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            RouteAction::List => {
                self.eval_bridge(
                    "return window.__VICTAURI__?.getRouteRules()",
                    params.webview_label.as_deref(),
                )
                .await
            }
            RouteAction::Clear => {
                let Some(id) = params.id else {
                    return missing_param("id", "clear");
                };
                let code = format!("return window.__VICTAURI__?.clearRoute({id})");
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            RouteAction::ClearAll => {
                self.eval_bridge(
                    "return window.__VICTAURI__?.clearRoutes()",
                    params.webview_label.as_deref(),
                )
                .await
            }
            RouteAction::Matches => {
                let limit = params.limit.unwrap_or(100);
                let code = format!("return window.__VICTAURI__?.getRouteMatches({limit})");
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
        }
    }

    #[tool(
        description = "Screencast / visual trace (no CDP). Captures the window at a fixed interval \
            into a ring buffer, forming a visual timeline that pairs with `recording` (events) and \
            `logs` (network/console). Actions:\n\
            - `start`: begin capturing (`interval_ms` default 500, `max_frames` default 60). Set \
              `with_events=true` to also start the event recorder.\n\
            - `stop`: stop and return a summary (frame count, duration, timestamps).\n\
            - `status`: active flag + buffered frame count.\n\
            - `frames`: return captured frames as base64 PNGs (`limit` caps how many).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn trace(&self, Parameters(params): Parameters<TraceParams>) -> CallToolResult {
        if !self.state.privacy.is_tool_enabled("trace")
            || !self.state.privacy.is_tool_enabled("screenshot")
        {
            return tool_disabled("trace");
        }
        match params.action {
            TraceAction::Start => {
                let interval = params.interval_ms.unwrap_or(500);
                let max_frames = params.max_frames.unwrap_or(60);
                let label = params.webview_label.clone();
                let generation = self
                    .state
                    .screencast
                    .start(interval, max_frames, label.clone());

                let mut events_started = false;
                if params.with_events.unwrap_or(false) {
                    let session_id = uuid::Uuid::new_v4().to_string();
                    if self.state.recorder.start(session_id).is_ok() {
                        events_started = true;
                    }
                }

                // Background capture task: snapshot the window each interval until
                // the screencast is stopped (or superseded by a newer start).
                let bridge = self.bridge.clone();
                let screencast = self.state.screencast.clone();
                tokio::spawn(async move {
                    let t0 = std::time::Instant::now();
                    while screencast.is_active() && screencast.generation() == generation {
                        if let Ok(handle) = bridge.get_native_handle(label.as_deref())
                            && let Ok(png) = crate::screenshot::capture_window(handle).await
                        {
                            use base64::Engine;
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
                            #[allow(clippy::cast_possible_truncation)]
                            screencast.push_frame(t0.elapsed().as_millis() as u64, b64);
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(
                            screencast.interval_ms(),
                        ))
                        .await;
                    }
                });

                json_result(&serde_json::json!({
                    "started": true,
                    "interval_ms": interval.max(50),
                    "max_frames": max_frames.clamp(1, 600),
                    "with_events": events_started,
                }))
            }
            TraceAction::Stop => {
                let frame_count = self.state.screencast.stop();
                let timestamps = self.state.screencast.frame_timestamps();
                let duration_ms = timestamps.last().copied().unwrap_or(0);
                let event_count = self.state.recorder.event_count();
                json_result(&serde_json::json!({
                    "stopped": true,
                    "frame_count": frame_count,
                    "duration_ms": duration_ms,
                    "frame_timestamps_ms": timestamps,
                    "recorded_event_count": event_count,
                    "hint": "use action=frames to retrieve PNGs; pair with recording/get_events and logs for a full bundle",
                }))
            }
            TraceAction::Status => json_result(&serde_json::json!({
                "active": self.state.screencast.is_active(),
                "frame_count": self.state.screencast.frame_count(),
                "interval_ms": self.state.screencast.interval_ms(),
            })),
            TraceAction::Frames => {
                let limit = params.limit.unwrap_or(0);
                let frames = self.state.screencast.frames(limit);
                let items: Vec<Content> = frames
                    .into_iter()
                    .map(|f| Content::image(f.data_b64, "image/png"))
                    .collect();
                if items.is_empty() {
                    return json_result(&serde_json::json!({ "frames": 0 }));
                }
                CallToolResult::success(items)
            }
        }
    }

    #[tool(
        description = "Animation introspection (no CDP). Reads the Web Animations API to reveal what \
            the webview's animation engine is actually running — duration, delay, easing, iterations, \
            keyframes, current progress, and the animating element. Standard DOM, so it works \
            identically on WebView2/WKWebView/WebKitGTK. Actions:\n\
            - `list`: return all running CSS animations/transitions (optionally scoped by `selector`), \
              each with declared `timing`, `computed` progress, `keyframes`, and `target`.\n\
            - `scrub`: deterministically pause the target's animation and seek it to `points` \
              evenly-spaced steps (default 20), returning the exact geometry curve (rect + transform \
              + opacity per step). With `capture=true`, also returns a single contact-sheet filmstrip \
              PNG (one image of the whole arc) plus a `manifest` mapping each cell to its progress/time. \
              Frozen frames are jank-free, so this beats real-time capture for fast sweeps. CSS-driven \
              animations only (JS/rAF animations are not seekable — use `list`/`sample`).\n\
            - `sample`: real-time motion recorder. `record=true` arms a requestAnimationFrame watcher \
              on `selector` (or the first animating element); then trigger the animation; then call \
              with `record=false` to read the measured per-frame curve plus jank stats (dropped frames, \
              max frame gap) and declared-vs-measured duration. Works for ANY animation including \
              JS/rAF-driven ones. `clear=true` resets recorded sessions.\n\
            NOTE: an animation only appears while it is running or pending — trigger it (e.g. show the \
            notification) just before calling `list`/`scrub`, or arm `sample` before triggering.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn animation(&self, Parameters(params): Parameters<AnimationParams>) -> CallToolResult {
        if !self.state.privacy.is_tool_enabled("animation") {
            return tool_disabled("animation");
        }
        match params.action {
            AnimationAction::List => {
                let sel = params
                    .selector
                    .as_deref()
                    .map_or_else(|| "null".to_string(), js_string);
                let code = format!(
                    "return window.__VICTAURI__ && window.__VICTAURI__.listAnimations({sel})"
                );
                match self
                    .eval_with_return(&code, params.webview_label.as_deref())
                    .await
                {
                    Ok(result_str) => {
                        match serde_json::from_str::<serde_json::Value>(&result_str) {
                            Ok(v) => json_result(&v),
                            Err(_) => CallToolResult::success(vec![Content::text(result_str)]),
                        }
                    }
                    Err(e) => tool_error(format!("animation list failed: {e}")),
                }
            }
            AnimationAction::Scrub => self.animation_scrub(params).await,
            AnimationAction::Sample => {
                let label = params.webview_label.as_deref();
                let sel = params
                    .selector
                    .as_deref()
                    .map_or_else(|| "null".to_string(), js_string);
                let code = if params.record.unwrap_or(false) {
                    format!("return window.__VICTAURI__.installSweepRecorder({sel})")
                } else {
                    let clear = params.clear.unwrap_or(false);
                    format!("return window.__VICTAURI__.readSweep({clear})")
                };
                match self.eval_with_return(&code, label).await {
                    Ok(result_str) => {
                        match serde_json::from_str::<serde_json::Value>(&result_str) {
                            Ok(v) => json_result(&v),
                            Err(_) => CallToolResult::success(vec![Content::text(result_str)]),
                        }
                    }
                    Err(e) => tool_error(format!("animation sample failed: {e}")),
                }
            }
        }
    }

    /// Deterministic pause-seek-capture loop for `animation scrub`. Split out to
    /// keep the `#[tool]` method readable.
    async fn animation_scrub(&self, params: AnimationParams) -> CallToolResult {
        let label = params.webview_label.as_deref();
        let sel = params
            .selector
            .as_deref()
            .map_or_else(|| "null".to_string(), js_string);

        // 1. Prepare: pause the target's animations, learn the timeline length.
        let prep_code = format!("return await window.__VICTAURI__.scrubPrepare({sel})");
        let prep_v = match self.eval_with_return(&prep_code, label).await {
            Ok(s) => {
                serde_json::from_str::<serde_json::Value>(&s).unwrap_or(serde_json::Value::Null)
            }
            Err(e) => return tool_error(format!("scrub prepare failed: {e}")),
        };
        if prep_v.get("prepared").and_then(serde_json::Value::as_bool) != Some(true) {
            // Surface the helpful error/info object (no target, JS-driven, etc.).
            return json_result(&prep_v);
        }

        let points = params.points.unwrap_or(20).clamp(2, 120);
        let capture = params.capture.unwrap_or(false);
        let mut curve: Vec<serde_json::Value> = Vec::with_capacity(points);
        let mut frames: Vec<crate::filmstrip::Frame> = Vec::new();
        let mut manifest: Vec<serde_json::Value> = Vec::new();

        // 2. Seek to each evenly-spaced point; capture the frozen frame if asked.
        for i in 0..points {
            #[allow(clippy::cast_precision_loss)]
            let progress = i as f64 / (points - 1) as f64;
            let seek_code = format!("return await window.__VICTAURI__.scrubSeek({progress})");
            match self.eval_with_return(&seek_code, label).await {
                Ok(s) => {
                    let v = serde_json::from_str::<serde_json::Value>(&s)
                        .unwrap_or(serde_json::Value::Null);
                    if capture
                        && let Ok(handle) = self.bridge.get_native_handle(label)
                        && let Ok((rgba, w, h)) =
                            crate::screenshot::capture_window_raw(handle).await
                        && let Some(frame) = crate::filmstrip::Frame::new(rgba, w, h)
                    {
                        manifest.push(serde_json::json!({
                            "cell": frames.len(),
                            "progress": progress,
                            "t": v.get("t").cloned().unwrap_or(serde_json::Value::Null),
                        }));
                        frames.push(frame);
                    }
                    curve.push(v);
                }
                Err(e) => curve.push(serde_json::json!({ "progress": progress, "error": e })),
            }
        }

        // 3. Restore (resume) or leave paused.
        let resume = params.restore.unwrap_or(true);
        let restore_code = format!("return window.__VICTAURI__.scrubRestore({resume})");
        let _ = self.eval_with_return(&restore_code, label).await;

        let mut meta = serde_json::json!({
            "scrubbed": true,
            "points": points,
            "duration_ms": prep_v.get("duration").cloned().unwrap_or(serde_json::Value::Null),
            "anim_count": prep_v.get("anim_count").cloned().unwrap_or(serde_json::Value::Null),
            "target": prep_v.get("target").cloned().unwrap_or(serde_json::Value::Null),
            "captured": capture,
            "curve": curve,
        });

        // 4. Compose the filmstrip if we captured frames.
        if capture && !frames.is_empty() {
            let cols = params
                .cols
                .unwrap_or_else(|| crate::filmstrip::default_cols(frames.len()));
            if let Some((rgba, w, h)) =
                crate::filmstrip::compose(&frames, cols, 4, [20, 20, 20, 255])
            {
                match crate::screenshot::encode_png(w, h, &rgba) {
                    Ok(png) => {
                        use base64::Engine;
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
                        meta["filmstrip"] = serde_json::json!({
                            "cols": cols,
                            "frame_count": frames.len(),
                            "width": w,
                            "height": h,
                            "manifest": manifest,
                        });
                        return CallToolResult::success(vec![
                            Content::image(b64, "image/png"),
                            Content::text(meta.to_string()),
                        ]);
                    }
                    Err(e) => return tool_error(format!("filmstrip encode failed: {e}")),
                }
            }
        }

        json_result(&meta)
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
                let base = if since_arg.is_empty() {
                    "window.__VICTAURI__?.getConsoleLogs()".to_string()
                } else {
                    format!("window.__VICTAURI__?.getConsoleLogs({since_arg})")
                };
                let code = if let Some(limit) = params.limit {
                    format!("return ({base} || []).slice(-{limit})")
                } else {
                    format!("return {base}")
                };
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            LogsAction::Network => {
                let filter_arg = params
                    .filter
                    .as_ref()
                    .map_or_else(|| "null".to_string(), |f| js_string(f));
                let limit = params.limit.unwrap_or(DEFAULT_LOG_LIMIT);
                let source = format!("window.__VICTAURI__?.getNetworkLog({filter_arg}, {limit})");
                let code = trimmed_log_js(&source, limit);
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            LogsAction::Ipc => {
                let wait = params.wait_for_capture.unwrap_or(false);
                let limit = params.limit.unwrap_or(DEFAULT_LOG_LIMIT);
                if wait {
                    let inner = trimmed_log_js("window.__VICTAURI__.getIpcLog()", limit);
                    let code = format!(
                        r"return (async function() {{
                            await window.__VICTAURI__.waitForIpcComplete(500);
                            return (function() {{ {inner} }})();
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
                    let code = trimmed_log_js("window.__VICTAURI__?.getIpcLog()", limit);
                    self.eval_bridge(&code, params.webview_label.as_deref())
                        .await
                }
            }
            LogsAction::Navigation => {
                let code = if let Some(limit) = params.limit {
                    format!(
                        "return (window.__VICTAURI__?.getNavigationLog() || []).slice(-{limit})"
                    )
                } else {
                    "return window.__VICTAURI__?.getNavigationLog()".to_string()
                };
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            LogsAction::Dialogs => {
                let code = if let Some(limit) = params.limit {
                    format!("return (window.__VICTAURI__?.getDialogLog() || []).slice(-{limit})")
                } else {
                    "return window.__VICTAURI__?.getDialogLog()".to_string()
                };
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            LogsAction::Events => {
                let since_arg = params.since.map(|ts| format!("{ts}")).unwrap_or_default();
                let base = if since_arg.is_empty() {
                    "window.__VICTAURI__?.getEventStream()".to_string()
                } else {
                    format!("window.__VICTAURI__?.getEventStream({since_arg})")
                };
                let code = if let Some(limit) = params.limit {
                    format!("return ({base} || []).slice(-{limit})")
                } else {
                    format!("return {base}")
                };
                self.eval_bridge(&code, params.webview_label.as_deref())
                    .await
            }
            LogsAction::SlowIpc => {
                let Some(threshold) = params.threshold_ms else {
                    return missing_param("threshold_ms", "slow_ipc");
                };
                let limit = params.limit.unwrap_or(20);
                let mb = MAX_LOG_FIELD_BYTES;
                let code = format!(
                    r"return (function() {{
                        var MB = {mb};
                        function trimField(v) {{
                            if (typeof v === 'string') return v.length > MB ? (v.slice(0, MB) + '…[+' + (v.length - MB) + ' bytes truncated]') : v;
                            if (v && typeof v === 'object') {{ var s; try {{ s = JSON.stringify(v); }} catch (e) {{ s = ''; }} if (s.length > MB) return '[truncated ' + s.length + ' bytes]'; }}
                            return v;
                        }}
                        function trimEntry(e) {{ if (e == null || typeof e !== 'object') return e; var o = {{}}; for (var k in e) {{ if (Object.prototype.hasOwnProperty.call(e, k)) o[k] = trimField(e[k]); }} return o; }}
                        var log = window.__VICTAURI__?.getIpcLog() || [];
                        var slow = log.filter(function(c) {{ return (c.duration_ms || 0) > {threshold}; }});
                        slow.sort(function(a, b) {{ return (b.duration_ms || 0) - (a.duration_ms || 0); }});
                        return {{ threshold_ms: {threshold}, count: Math.min(slow.length, {limit}), calls: slow.slice(0, {limit}).map(trimEntry) }};
                    }})()",
                );
                self.eval_bridge(&code, None).await
            }
            LogsAction::Clear => {
                // Clearing the IPC/network logs erases captured evidence — a
                // mutation of observable state — so it is gated separately and
                // excluded from the read-only Observe profile (red-team P1).
                if !self.state.privacy.is_tool_enabled("logs.clear") {
                    return tool_disabled("logs.clear");
                }
                let code = "return (function(){ var b = window.__VICTAURI__; if (!b) return { ok:false, error:'bridge unavailable' }; if (b.clearIpcLog) b.clearIpcLog(); if (b.clearNetworkLog) b.clearNetworkLog(); return { ok:true, cleared:['ipc','network'] }; })()";
                self.eval_bridge(code, params.webview_label.as_deref())
                    .await
            }
        }
    }

    // ── Backend Introspection ────────────────────────────────────────────────

    #[tool(
        description = "Deep backend introspection — command profiling, IPC contract testing, \
            coverage, startup timing, capability auditing, database diagnostics, process \
            enumeration, and event bus monitoring. \
            These features exploit Victauri's position inside the Rust process.\n\n\
            Actions:\n\
            - `command_timings`: Per-command execution timing stats (min/max/avg/p95). Set `slow_threshold_ms` to filter.\n\
            - `coverage`: Which registered commands have been called during this session.\n\
            - `command_catalog`: Per-command argument + result SHAPES mined from the live IPC log, merged with the registry — real call/return schemas even for apps that don't use #[inspectable] (where get_registry is names-only). The highest-signal way to learn how to drive an app's commands.\n\
            - `contract_record`: Record a command's response shape as a baseline (requires `command`).\n\
            - `contract_check`: Check all recorded contracts for schema drift.\n\
            - `contract_list`: List all recorded contract baselines.\n\
            - `contract_clear`: Clear all recorded contract baselines.\n\
            - `startup_timing`: Victauri plugin initialization phase-by-phase timing breakdown.\n\
            - `capabilities`: Enumerate Tauri v2 capabilities, security config (CSP, freeze_prototype), configured plugins, and window definitions.\n\
            - `db_health`: Read-only SQLite database diagnostics (journal mode, WAL presence, page stats).\n\
            - `plugin_state`: Snapshot of the Victauri plugin's internal state (event log, registry, faults, recording, timings, etc.).\n\
            - `processes`: Enumerate the host process and all child processes (sidecars, background workers) with PID, name, and memory usage.\n\
            - `plugin_tasks`: List Victauri's own spawned async tasks (MCP server, event drain) with status.\n\
            - `event_bus`: List captured Tauri events + app events (auto-intercepted via listen_any — no app opt-in needed). Returns the newest events per category (default 100) so the full buffers (up to ~11k events / megabytes) never overflow the result; `count` is the true total and `truncated` flags a capped slice. Scope via the `args` object: `{\"action\":\"event_bus\",\"args\":{\"limit\":500,\"since_ms\":5000}}`.\n\
            - `event_bus_clear`: Clear the event bus capture buffer.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn introspect(&self, Parameters(params): Parameters<IntrospectParams>) -> CallToolResult {
        if !self.state.privacy.is_tool_enabled("introspect") {
            return tool_disabled("introspect");
        }

        match params.action {
            IntrospectAction::CommandTimings => {
                let mut stats = self.state.command_timings.all_stats();
                let driven_count = stats.len();
                if let Some(threshold) = params.slow_threshold_ms {
                    stats.retain(|s| s.avg_ms >= threshold);
                }

                // Real frontend traffic: derive per-command latency from the live IPC
                // log so the profiler is not blind to commands the app itself drives.
                // `command_timings` (above) only records Victauri-driven invoke_command
                // calls — on a running app that counter is typically 0 while the app
                // makes hundreds of real calls. The IPC log captures those with
                // duration; the name+duration projection stays under the eval cap.
                let code = ipc_timing_projection_js(None);
                let mut ipc_traffic = match self
                    .eval_with_return(&code, params.webview_label.as_deref())
                    .await
                {
                    Ok(json_str) => serde_json::from_str::<Vec<serde_json::Value>>(&json_str)
                        .map(|entries| ipc_timing_stats(&entries))
                        .unwrap_or_default(),
                    Err(_) => Vec::new(),
                };
                if let Some(threshold) = params.slow_threshold_ms {
                    ipc_traffic.retain(|s| {
                        s.get("avg_ms")
                            .and_then(serde_json::Value::as_f64)
                            .is_some_and(|a| a >= threshold)
                    });
                }

                let result = serde_json::json!({
                    "commands": stats,
                    "total_commands_profiled": driven_count,
                    "ipc_traffic": ipc_traffic,
                    "ipc_commands_observed": ipc_traffic.len(),
                    "slow_threshold_ms": params.slow_threshold_ms,
                    "note": "`commands` profiles ONLY commands you drove through Victauri's \
                             invoke_command tool (often empty on a live app). `ipc_traffic` \
                             profiles the app's REAL frontend IPC, derived from the live IPC \
                             log (per-command call_count + min/max/avg/p95 latency) — that is \
                             the one reflecting actual usage.",
                });
                json_result(&result)
            }
            IntrospectAction::Coverage => {
                let registered: Vec<String> = self
                    .state
                    .registry
                    .list()
                    .iter()
                    .map(|c| c.name.clone())
                    .collect();

                // Project to command NAMES ONLY. The previous full `getIpcLog()` carried
                // request/response bodies and blew the eval result cap on busy apps,
                // silently returning an empty set and reporting "0 invoked" despite live
                // traffic. This is the same name projection ghost detection uses.
                let code = ghost_ipc_projection_js(None);
                let (invoked, ipc_calls_observed): (std::collections::HashSet<String>, usize) =
                    match self
                        .eval_with_return(&code, params.webview_label.as_deref())
                        .await
                    {
                        Ok(json_str) => match serde_json::from_str::<Vec<String>>(&json_str) {
                            Ok(names) => {
                                let count = names.len();
                                (names.into_iter().collect(), count)
                            }
                            Err(_) => (std::collections::HashSet::new(), 0),
                        },
                        Err(_) => (std::collections::HashSet::new(), 0),
                    };

                let uncovered: Vec<&String> = registered
                    .iter()
                    .filter(|cmd| !invoked.contains(cmd.as_str()))
                    .collect();

                let coverage_pct = if registered.is_empty() {
                    100.0
                } else {
                    let covered = registered.len() - uncovered.len();
                    (covered as f64 / registered.len() as f64) * 100.0
                };

                let note = if registered.is_empty() {
                    Some(
                        "The introspection registry is empty (the app does not use \
                         #[inspectable]/register_command_names), so coverage_pct is a \
                         placeholder 100%. `invoked_not_registered` still lists the real \
                         commands seen on the live IPC log — use it to inventory actual \
                         traffic.",
                    )
                } else if ipc_calls_observed == 0 {
                    Some(
                        "No IPC calls were observed on the live log. If the app is actively \
                         making calls, confirm the target webview and that Tauri IPC routes \
                         through fetch to ipc.localhost (some commands use the native channel).",
                    )
                } else {
                    None
                };

                let result = serde_json::json!({
                    "registered_commands": registered.len(),
                    "invoked_commands": invoked.len(),
                    "ipc_calls_observed": ipc_calls_observed,
                    "coverage_pct": (coverage_pct * 10.0).round() / 10.0,
                    "uncovered": uncovered,
                    "invoked_not_registered": invoked.iter()
                        .filter(|cmd| !registered.contains(cmd))
                        .collect::<Vec<_>>(),
                    "note": note,
                });
                json_result(&result)
            }
            IntrospectAction::CommandCatalog => {
                // Mine the live IPC log for per-command argument + result SHAPES (inferred
                // in JS, bodies never shipped — so it stays under the eval cap on busy apps)
                // and merge with the #[inspectable] registry. This is the answer to a real
                // live gap: an app without #[inspectable] (e.g. 4DA — 379 commands, every
                // registry field null) gives an agent command NAMES but no call/return
                // schema; the IPC log holds the actual shapes, so we project them out.
                let code = ipc_catalog_projection_js();
                let ipc_entries: Vec<serde_json::Value> = match self
                    .eval_with_return(&code, params.webview_label.as_deref())
                    .await
                {
                    Ok(json_str) => serde_json::from_str(&json_str).unwrap_or_default(),
                    Err(e) => return tool_error(format!("failed to read IPC log: {e}")),
                };

                let registry = self.state.registry.list();
                let catalog = merge_command_catalog(&ipc_entries, &registry);
                let observed = catalog
                    .iter()
                    .filter(|c| {
                        c.get("observed")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false)
                    })
                    .count();

                let result = serde_json::json!({
                    "catalog": catalog,
                    "observed_count": observed,
                    "registered_count": registry.len(),
                    "total": catalog.len(),
                    "note": "`arg_shape`/`result_shape` are STRUCTURES inferred from the live \
                             IPC log (keys + value types, not values) — how to CALL each command \
                             and what it RETURNS, even for apps that don't use #[inspectable]. \
                             `observed:false` means the command is in the registry but hasn't \
                             been seen on the wire this session (drive the app's UI to populate \
                             its shape). `declared_*` fields, when present, come from \
                             #[inspectable] and are authoritative over the inferred shape.",
                });
                json_result(&result)
            }
            IntrospectAction::ContractRecord => {
                let Some(command) = params.command else {
                    return missing_param("command", "contract_record");
                };
                // contract_record invokes the command with caller-supplied args, so
                // it must honour the same allow/blocklist as invoke_command (audit #30).
                if !self.state.privacy.is_invoke_allowed(&command)
                    || !self.state.privacy.is_command_allowed(&command)
                {
                    return tool_error(format!(
                        "command '{command}' is blocked by privacy configuration"
                    ));
                }
                let args_json = params.args.unwrap_or(serde_json::json!({}));
                let args_str =
                    serde_json::to_string(&args_json).unwrap_or_else(|_| "{}".to_string());
                let code = format!(
                    "return window.__TAURI_INTERNALS__.invoke({}, {args_str})",
                    js_string(&command)
                );
                match self
                    .eval_with_return(&code, params.webview_label.as_deref())
                    .await
                {
                    Ok(result_str) => {
                        let value: serde_json::Value = serde_json::from_str(&result_str)
                            .unwrap_or(serde_json::Value::String(result_str.clone()));
                        let shape = crate::introspection::JsonShape::from_value(&value);
                        let sample = if result_str.len() > 4096 {
                            format!("{}...(truncated)", &result_str[..4096])
                        } else {
                            result_str
                        };
                        let baseline = crate::introspection::ContractBaseline {
                            command: command.clone(),
                            args: args_json,
                            shape: shape.clone(),
                            sample,
                            recorded_at: chrono_now(),
                        };
                        self.state.contract_store.record(baseline);
                        let result = serde_json::json!({
                            "recorded": true,
                            "command": command,
                            "shape_type": shape.type_name(),
                        });
                        json_result(&result)
                    }
                    Err(e) => tool_error(format!(
                        "failed to invoke '{command}' for contract recording: {e}"
                    )),
                }
            }
            IntrospectAction::ContractCheck => {
                let baselines = self.state.contract_store.all();
                if baselines.is_empty() {
                    return json_result(&serde_json::json!({
                        "checked": 0,
                        "message": "no contract baselines recorded — use contract_record first",
                    }));
                }
                let mut results = Vec::new();
                for baseline in &baselines {
                    // Re-checking a baseline re-invokes the command; honour the
                    // allow/blocklist in case it changed since recording (audit #30).
                    if !self.state.privacy.is_invoke_allowed(&baseline.command)
                        || !self.state.privacy.is_command_allowed(&baseline.command)
                    {
                        continue;
                    }
                    let args_str =
                        serde_json::to_string(&baseline.args).unwrap_or_else(|_| "{}".to_string());
                    let code = format!(
                        "return window.__TAURI_INTERNALS__.invoke({}, {args_str})",
                        js_string(&baseline.command)
                    );
                    match self
                        .eval_with_return(&code, params.webview_label.as_deref())
                        .await
                    {
                        Ok(result_str) => {
                            let value: serde_json::Value = serde_json::from_str(&result_str)
                                .unwrap_or(serde_json::Value::String(result_str));
                            let current_shape = crate::introspection::JsonShape::from_value(&value);
                            let drift = crate::introspection::diff_shapes(
                                &baseline.shape,
                                &current_shape,
                                &baseline.command,
                            );
                            results.push(drift);
                        }
                        Err(e) => {
                            results.push(crate::introspection::ContractDrift {
                                command: baseline.command.clone(),
                                new_fields: Vec::new(),
                                removed_fields: Vec::new(),
                                type_changes: Vec::new(),
                                shape_matches: false,
                            });
                            tracing::warn!(
                                command = %baseline.command,
                                error = %e,
                                "contract check invocation failed"
                            );
                        }
                    }
                }
                let passing = results.iter().filter(|r| r.shape_matches).count();
                let result = serde_json::json!({
                    "checked": results.len(),
                    "passing": passing,
                    "failing": results.len() - passing,
                    "contracts": results,
                });
                json_result(&result)
            }
            IntrospectAction::ContractList => {
                let baselines = self.state.contract_store.all();
                let result = serde_json::json!({
                    "count": baselines.len(),
                    "baselines": baselines.iter().map(|b| serde_json::json!({
                        "command": b.command,
                        "shape_type": b.shape.type_name(),
                        "recorded_at": b.recorded_at,
                    })).collect::<Vec<_>>(),
                });
                json_result(&result)
            }
            IntrospectAction::ContractClear => {
                let cleared = self.state.contract_store.clear();
                json_result(&serde_json::json!({
                    "cleared": cleared,
                }))
            }
            IntrospectAction::StartupTiming => {
                let phases = self.state.startup_timeline.report();
                let result = serde_json::json!({
                    "phases": phases,
                    "total_ms": self.state.startup_timeline.total_ms(),
                    "uptime_secs": self.state.started_at.elapsed().as_secs(),
                });
                json_result(&result)
            }
            IntrospectAction::Capabilities => {
                let config = self.bridge.tauri_config();
                let live_windows = self.bridge.list_window_labels();

                let result = serde_json::json!({
                    "app": {
                        "identifier": config.get("identifier"),
                        "product_name": config.get("product_name"),
                        "version": config.get("version"),
                    },
                    "security": config.get("security"),
                    "configured_windows": config.get("windows"),
                    "live_windows": live_windows,
                    "configured_plugins": config.get("plugins"),
                    "victauri": {
                        "registered_commands": self.state.registry.list().len(),
                        "redaction_enabled": self.state.privacy.redaction_enabled,
                        "privacy_profile": format!("{:?}", self.state.privacy.profile),
                        "disabled_tools": &self.state.privacy.disabled_tools,
                    },
                });
                json_result(&result)
            }
            #[allow(unused_variables)]
            IntrospectAction::DbHealth => {
                #[cfg(feature = "sqlite")]
                {
                    let db_path = params.db_path.clone();
                    match self.run_db_health(db_path.as_deref()).await {
                        Ok(health) => json_result(&health),
                        Err(e) => tool_error(format!("db_health failed: {e}")),
                    }
                }
                #[cfg(not(feature = "sqlite"))]
                {
                    tool_error("SQLite support not compiled in — enable the `sqlite` feature")
                }
            }
            IntrospectAction::PluginState => {
                let recording_active = self.state.recorder.is_recording();
                let recording_events = self.state.recorder.event_count();
                let result = serde_json::json!({
                    "event_log": {
                        "size": self.state.event_log.len(),
                        "capacity": self.state.event_log.capacity(),
                    },
                    "registry": {
                        "commands_registered": self.state.registry.list().len(),
                    },
                    "recording": {
                        "active": recording_active,
                        "events_captured": recording_events,
                    },
                    "faults": {
                        "active_rules": self.state.fault_registry.list().len(),
                    },
                    "contracts": {
                        "baselines_recorded": self.state.contract_store.all().len(),
                    },
                    "timings": {
                        "commands_profiled": self.state.command_timings.all_stats().len(),
                    },
                    "event_bus": {
                        "captured_events": self.state.event_bus.len(),
                    },
                    "tasks": {
                        "total": self.state.task_tracker.list().len(),
                        "active": self.state.task_tracker.active_count(),
                    },
                    "tool_invocations": self.state.tool_invocations.load(Ordering::Relaxed),
                    "uptime_secs": self.state.started_at.elapsed().as_secs(),
                    "port": self.state.port.load(std::sync::atomic::Ordering::Relaxed),
                });
                json_result(&result)
            }
            IntrospectAction::Processes => {
                let pid = std::process::id();
                let uptime = self.state.started_at.elapsed();
                let children = crate::introspection::enumerate_child_processes();
                let host_memory = crate::memory::current_stats();

                let result = serde_json::json!({
                    "host": {
                        "pid": pid,
                        "uptime_secs": uptime.as_secs(),
                        "platform": std::env::consts::OS,
                        "arch": std::env::consts::ARCH,
                        "memory": host_memory,
                    },
                    "children": children.iter().map(|c| serde_json::json!({
                        "pid": c.pid,
                        "name": c.name,
                        "memory_bytes": c.memory_bytes,
                    })).collect::<Vec<_>>(),
                    "child_count": children.len(),
                    "total_child_memory_bytes": children.iter().filter_map(|c| c.memory_bytes).sum::<u64>(),
                });
                json_result(&result)
            }
            IntrospectAction::PluginTasks => {
                let tasks = self.state.task_tracker.list();
                let active = self.state.task_tracker.active_count();
                let result = serde_json::json!({
                    "total": tasks.len(),
                    "active": active,
                    "finished": tasks.len() - active,
                    "tasks": tasks,
                });
                json_result(&result)
            }
            IntrospectAction::EventBus => {
                // Default cap so the full buffers (up to 1k Tauri + 10k app events, often
                // megabytes / tens of thousands of lines) can never overflow the tool result
                // cap (VIC-4). Newest events first; `count` is the full total so a truncated
                // slice is always diagnosable. Optional `limit` / `since_ms` are read from the
                // generic `args` object (a dedicated public field would be a semver-major break).
                let opts = params.args.as_ref();
                let limit = opts
                    .and_then(|a| a.get("limit"))
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(100);
                let since_ms = opts
                    .and_then(|a| a.get("since_ms"))
                    .and_then(serde_json::Value::as_u64);
                let cutoff = since_ms.map(|ms| {
                    chrono::Utc::now()
                        - chrono::TimeDelta::milliseconds(i64::try_from(ms).unwrap_or(i64::MAX))
                });

                let all_tauri = self.state.event_bus.events();
                let tauri_total = all_tauri.len();
                let tauri_matched: Vec<_> = all_tauri
                    .into_iter()
                    .filter(|e| match cutoff {
                        Some(cut) => chrono::DateTime::parse_from_rfc3339(&e.timestamp)
                            .map_or(true, |t| t.with_timezone(&chrono::Utc) >= cut),
                        None => true,
                    })
                    .collect();
                let tauri_matched_count = tauri_matched.len();
                let tauri_events: Vec<_> = tauri_matched.into_iter().rev().take(limit).collect();

                // Exclude Victauri's own infrastructure events (plugin:victauri|* IPC etc.) —
                // noise in a diagnostic timeline; the `explain` tools already filter them via
                // `is_internal()`.
                let all_app: Vec<_> = self
                    .state
                    .event_log
                    .snapshot()
                    .into_iter()
                    .filter(|e| !e.is_internal())
                    .collect();
                let app_total = all_app.len();
                let app_matched: Vec<_> = match cutoff {
                    Some(cut) => all_app
                        .into_iter()
                        .filter(|e| e.timestamp() >= cut)
                        .collect(),
                    None => all_app,
                };
                let app_matched_count = app_matched.len();
                let app_events: Vec<_> = app_matched.into_iter().rev().take(limit).collect();

                let result = serde_json::json!({
                    "limit": limit,
                    "since_ms": since_ms,
                    "tauri_events": {
                        "count": tauri_total,
                        "matched": tauri_matched_count,
                        "returned": tauri_events.len(),
                        "truncated": tauri_matched_count > tauri_events.len(),
                        "events": tauri_events,
                    },
                    "app_events": {
                        "count": app_total,
                        "matched": app_matched_count,
                        "returned": app_events.len(),
                        "truncated": app_matched_count > app_events.len(),
                        "capacity": self.state.event_log.capacity(),
                        "events": app_events,
                    },
                });
                json_result(&result)
            }
            IntrospectAction::EventBusClear => {
                let tauri_cleared = self.state.event_bus.clear();
                self.state.event_log.clear();
                json_result(&serde_json::json!({
                    "tauri_events_cleared": tauri_cleared,
                    "app_events_cleared": true,
                }))
            }
        }
    }

    // ── Fault Injection / Chaos Engineering ──────────────────────────────────

    #[tool(
        description = "Probe a backend command handler under failure by faulting it for chaos engineering. \
            Simulate slow commands, backend errors, dropped responses, and corrupted data. \
            SCOPE: faults apply ONLY to commands you run via this server's `invoke_command` tool — \
            they do NOT intercept the app's real user-driven IPC (window.__TAURI_INTERNALS__.invoke), \
            which runs below the layer Victauri can reach. Use this to test a handler's error path when \
            YOU drive it; it does not reproduce a failure a user clicking the UI would see.\n\n\
            Actions:\n\
            - `inject`: Add a fault rule (requires `command`, `fault_type`). Optional: `delay_ms`, `error_message`, `max_triggers`.\n\
            - `list`: List all active fault injection rules.\n\
            - `clear`: Remove a specific fault rule (requires `command`).\n\
            - `clear_all`: Remove all fault rules.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn fault(&self, Parameters(params): Parameters<FaultParams>) -> CallToolResult {
        if !self.state.privacy.is_tool_enabled("fault") {
            return tool_disabled("fault");
        }

        match params.action {
            FaultAction::Inject => {
                let Some(command) = params.command else {
                    return missing_param("command", "inject");
                };
                let Some(fault_kind) = params.fault_type else {
                    return missing_param("fault_type", "inject");
                };
                let fault_type = match fault_kind {
                    FaultKind::Delay => {
                        let delay_ms = params.delay_ms.unwrap_or(1000);
                        crate::introspection::FaultType::Delay { delay_ms }
                    }
                    FaultKind::Error => {
                        let message = params
                            .error_message
                            .unwrap_or_else(|| "injected fault".to_string());
                        crate::introspection::FaultType::Error { message }
                    }
                    FaultKind::Drop => crate::introspection::FaultType::Drop,
                    FaultKind::Corrupt => crate::introspection::FaultType::Corrupt,
                };
                let config = crate::introspection::FaultConfig {
                    command: command.clone(),
                    fault_type: fault_type.clone(),
                    trigger_count: 0,
                    max_triggers: params.max_triggers.unwrap_or(0),
                    created_at: std::time::Instant::now(),
                };
                self.state.fault_registry.inject(config);
                let result = serde_json::json!({
                    "injected": true,
                    "command": command,
                    "fault_type": fault_type,
                    "max_triggers": params.max_triggers.unwrap_or(0),
                });
                json_result(&result)
            }
            FaultAction::List => {
                let faults = self.state.fault_registry.list();
                let result = serde_json::json!({
                    "count": faults.len(),
                    "faults": faults.iter().map(|f| serde_json::json!({
                        "command": f.command,
                        "fault_type": f.fault_type,
                        "trigger_count": f.trigger_count,
                        "max_triggers": f.max_triggers,
                    })).collect::<Vec<_>>(),
                });
                json_result(&result)
            }
            FaultAction::Clear => {
                let Some(command) = params.command else {
                    return missing_param("command", "clear");
                };
                let removed = self.state.fault_registry.clear(&command);
                json_result(&serde_json::json!({
                    "removed": removed,
                    "command": command,
                }))
            }
            FaultAction::ClearAll => {
                let removed = self.state.fault_registry.clear_all();
                json_result(&serde_json::json!({
                    "removed": removed,
                }))
            }
        }
    }

    // ── Cross-Layer Explanation ────────────────────────────────────────────

    #[tool(
        description = "Correlate recent activity across all layers into a coherent narrative. \
            CDP shows raw events per layer; Victauri correlates IPC + DOM + console + network \
            + window events across the Rust backend and webview simultaneously.\n\n\
            Actions:\n\
            - `summary`: High-level activity summary for the last N seconds (default 30). \
              Counts IPC calls, DOM mutations, console entries, network requests, errors.\n\
            - `last_action`: Correlate the most recent burst of events into a causal timeline \
              (e.g. 'IPC call → DOM update → console.log').\n\
            - `diff`: What changed in the last N seconds — event counts, errors, new IPC commands.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn explain(&self, Parameters(params): Parameters<ExplainParams>) -> CallToolResult {
        if !self.state.privacy.is_tool_enabled("explain") {
            return tool_disabled("explain");
        }

        match params.action {
            ExplainAction::Summary => {
                let secs = params.seconds.unwrap_or(30);
                let since = chrono::Utc::now()
                    - chrono::TimeDelta::try_seconds(secs as i64).unwrap_or_default();
                let events = self.state.event_log.since(since);

                let mut ipc_count = 0u64;
                let mut dom_mutations = 0u64;
                let mut state_changes = 0u64;
                let mut console_count = 0u64;
                let mut window_events = 0u64;
                let mut interactions = 0u64;
                let mut top_commands: HashMap<String, u64> = HashMap::new();
                let mut errors: Vec<String> = Vec::new();

                for event in &events {
                    match event {
                        victauri_core::AppEvent::Ipc(call) => {
                            ipc_count += 1;
                            *top_commands.entry(call.command.clone()).or_insert(0) += 1;
                            if let victauri_core::IpcResult::Err(e) = &call.result {
                                errors.push(format!("IPC {}: {e}", call.command));
                            }
                        }
                        victauri_core::AppEvent::DomMutation { mutation_count, .. } => {
                            dom_mutations += u64::from(*mutation_count)
                        }
                        victauri_core::AppEvent::StateChange { .. } => state_changes += 1,
                        victauri_core::AppEvent::Console { level, message, .. } => {
                            console_count += 1;
                            if level == "error" {
                                errors.push(format!("console.error: {message}"));
                            }
                        }
                        victauri_core::AppEvent::WindowEvent { .. } => window_events += 1,
                        victauri_core::AppEvent::DomInteraction { .. } => interactions += 1,
                        _ => {}
                    }
                }

                let mut sorted_cmds: Vec<_> = top_commands.into_iter().collect();
                sorted_cmds.sort_by_key(|b| std::cmp::Reverse(b.1));
                let top: Vec<_> = sorted_cmds.iter().take(5).collect();

                let narrative = format!(
                    "{ipc_count} IPC call{} in the last {secs}s{}. \
                     {dom_mutations} DOM mutation{}, {interactions} interaction{}, \
                     {console_count} console message{}, {window_events} window event{}. {}.",
                    if ipc_count == 1 { "" } else { "s" },
                    if top.is_empty() {
                        String::new()
                    } else {
                        format!(
                            ", dominated by {}",
                            top.iter()
                                .map(|(cmd, n)| format!("{cmd} ({n}x)"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    },
                    if dom_mutations == 1 { "" } else { "s" },
                    if interactions == 1 { "" } else { "s" },
                    if console_count == 1 { "" } else { "s" },
                    if window_events == 1 { "" } else { "s" },
                    if errors.is_empty() {
                        "No errors".to_string()
                    } else {
                        format!(
                            "{} error{}",
                            errors.len(),
                            if errors.len() == 1 { "" } else { "s" }
                        )
                    },
                );

                let result = serde_json::json!({
                    "time_window_secs": secs,
                    "total_events": events.len(),
                    "ipc_calls": ipc_count,
                    "dom_mutations": dom_mutations,
                    "state_changes": state_changes,
                    "console_messages": console_count,
                    "window_events": window_events,
                    "interactions": interactions,
                    "top_commands": sorted_cmds.iter().take(5).map(|(cmd, n)| {
                        serde_json::json!({"command": cmd, "count": n})
                    }).collect::<Vec<_>>(),
                    "errors": errors,
                    "narrative": narrative,
                });
                json_result(&result)
            }
            ExplainAction::LastAction => {
                let secs = params.seconds.unwrap_or(5);
                let since = chrono::Utc::now()
                    - chrono::TimeDelta::try_seconds(secs as i64).unwrap_or_default();
                let events = self.state.event_log.since(since);

                let timeline: Vec<serde_json::Value> = events
                    .iter()
                    .filter(|e| !e.is_internal())
                    .map(|event| match event {
                        victauri_core::AppEvent::Ipc(call) => serde_json::json!({
                            "time": call.timestamp.to_rfc3339_opts(
                                chrono::SecondsFormat::Millis, true
                            ),
                            "type": "ipc",
                            "detail": format!(
                                "{} {} ({}ms)",
                                call.command,
                                call.result,
                                call.duration_ms.unwrap_or(0)
                            ),
                        }),
                        victauri_core::AppEvent::DomMutation {
                            timestamp,
                            mutation_count,
                            webview_label,
                        } => serde_json::json!({
                            "time": timestamp.to_rfc3339_opts(
                                chrono::SecondsFormat::Millis, true
                            ),
                            "type": "dom_mutation",
                            "detail": format!(
                                "{mutation_count} element{} updated in {webview_label}",
                                if *mutation_count == 1 { "" } else { "s" }
                            ),
                        }),
                        victauri_core::AppEvent::DomInteraction {
                            timestamp,
                            action,
                            selector,
                            ..
                        } => serde_json::json!({
                            "time": timestamp.to_rfc3339_opts(
                                chrono::SecondsFormat::Millis, true
                            ),
                            "type": "interaction",
                            "detail": format!("{action} on {selector}"),
                        }),
                        victauri_core::AppEvent::StateChange {
                            timestamp,
                            key,
                            caused_by,
                        } => serde_json::json!({
                            "time": timestamp.to_rfc3339_opts(
                                chrono::SecondsFormat::Millis, true
                            ),
                            "type": "state_change",
                            "detail": format!(
                                "{key} changed{}",
                                caused_by.as_ref().map_or(String::new(), |c| format!(" (by {c})"))
                            ),
                        }),
                        victauri_core::AppEvent::Console {
                            timestamp,
                            level,
                            message,
                        } => serde_json::json!({
                            "time": timestamp.to_rfc3339_opts(
                                chrono::SecondsFormat::Millis, true
                            ),
                            "type": "console",
                            "detail": format!("console.{level}: {message}"),
                        }),
                        victauri_core::AppEvent::WindowEvent {
                            timestamp,
                            label,
                            event,
                        } => serde_json::json!({
                            "time": timestamp.to_rfc3339_opts(
                                chrono::SecondsFormat::Millis, true
                            ),
                            "type": "window_event",
                            "detail": format!("{event} on window '{label}'"),
                        }),
                        _ => serde_json::json!({
                            "time": event.timestamp().to_rfc3339_opts(
                                chrono::SecondsFormat::Millis, true
                            ),
                            "type": "other",
                            "detail": "unknown event type",
                        }),
                    })
                    .collect();

                let narrative = if timeline.is_empty() {
                    format!("No activity in the last {secs}s.")
                } else {
                    let parts: Vec<String> = timeline
                        .iter()
                        .filter_map(|e| e.get("detail").and_then(|d| d.as_str()))
                        .map(String::from)
                        .collect();
                    parts.join(" → ")
                };

                let result = serde_json::json!({
                    "time_window_secs": secs,
                    "event_count": timeline.len(),
                    "timeline": timeline,
                    "narrative": narrative,
                });
                json_result(&result)
            }
            ExplainAction::Diff => {
                let secs = params.seconds.unwrap_or(10);
                let since = chrono::Utc::now()
                    - chrono::TimeDelta::try_seconds(secs as i64).unwrap_or_default();
                let events = self.state.event_log.since(since);

                let mut ipc_commands: Vec<String> = Vec::new();
                let mut dom_changes = 0u64;
                let mut error_count = 0u64;
                let mut interaction_count = 0u64;
                let mut console_messages = 0u64;

                for event in &events {
                    if event.is_internal() {
                        continue;
                    }
                    match event {
                        victauri_core::AppEvent::Ipc(call) => {
                            ipc_commands.push(call.command.clone());
                            if matches!(call.result, victauri_core::IpcResult::Err(_)) {
                                error_count += 1;
                            }
                        }
                        victauri_core::AppEvent::DomMutation { mutation_count, .. } => {
                            dom_changes += u64::from(*mutation_count)
                        }
                        victauri_core::AppEvent::DomInteraction { .. } => {
                            interaction_count += 1;
                        }
                        victauri_core::AppEvent::Console { level, .. } => {
                            console_messages += 1;
                            if level == "error" {
                                error_count += 1;
                            }
                        }
                        _ => {}
                    }
                }

                ipc_commands.dedup();

                let result = serde_json::json!({
                    "since": since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    "time_window_secs": secs,
                    "total_events": events.len(),
                    "ipc_calls_made": ipc_commands.len(),
                    "unique_commands": ipc_commands,
                    "dom_elements_changed": dom_changes,
                    "interactions": interaction_count,
                    "console_messages": console_messages,
                    "errors": error_count,
                });
                json_result(&result)
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
            timed_out_labels: Arc::new(Mutex::new(HashSet::new())),
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
        // Centralized authorization: resolve the canonical `tool.action` capability
        // and gate on it BEFORE dispatch, so every compound action is checked
        // uniformly (not just the ones whose handler remembers to). See `authz`.
        let capability = authz::canonical_capability(name, &args);
        if !self.state.privacy.is_call_allowed(name, &capability) {
            return Ok(tool_disabled(&capability));
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
            "app_state" => {
                let p: AppStateParams = Self::parse_args(args)?;
                self.app_state(Parameters(p)).await
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
            "route" => {
                let p: RouteParams = Self::parse_args(args)?;
                self.route(Parameters(p)).await
            }
            "trace" => {
                let p: TraceParams = Self::parse_args(args)?;
                self.trace(Parameters(p)).await
            }
            "animation" => {
                let p: AnimationParams = Self::parse_args(args)?;
                self.animation(Parameters(p)).await
            }
            "logs" => {
                let p: LogsParams = Self::parse_args(args)?;
                self.logs(Parameters(p)).await
            }
            "introspect" => {
                let p: IntrospectParams = Self::parse_args(args)?;
                self.introspect(Parameters(p)).await
            }
            "fault" => {
                let p: FaultParams = Self::parse_args(args)?;
                self.fault(Parameters(p)).await
            }
            "explain" => {
                let p: ExplainParams = Self::parse_args(args)?;
                self.explain(Parameters(p)).await
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

    fn resolve_app_dir(&self, dir: Option<AppDir>) -> Result<std::path::PathBuf, String> {
        match dir.unwrap_or(AppDir::Data) {
            AppDir::Data => self.bridge.app_data_dir(),
            AppDir::Config => self.bridge.app_config_dir(),
            AppDir::Log => self.bridge.app_log_dir(),
            AppDir::LocalData => self.bridge.app_local_data_dir(),
        }
    }

    /// Lexical (pre-existence) traversal guard for a user-supplied sub-path.
    ///
    /// Rejects absolute paths and any component that is `..` BEFORE the path is
    /// canonicalized. This is necessary because [`Self::safe_within`] relies on
    /// `canonicalize`, which errors on non-existent paths — so a traversal
    /// attempt against a missing target would otherwise be reported as
    /// "not found" (an info-leak oracle) rather than as traversal.
    fn lexical_safe(sub: &std::path::Path) -> Result<(), String> {
        use std::path::Component;
        if sub.is_absolute() {
            return Err("path traversal not allowed: absolute paths are rejected".to_string());
        }
        for component in sub.components() {
            match component {
                Component::ParentDir => {
                    return Err("path traversal not allowed: '..' is rejected".to_string());
                }
                Component::Prefix(_) | Component::RootDir => {
                    return Err(
                        "path traversal not allowed: absolute paths are rejected".to_string()
                    );
                }
                Component::CurDir | Component::Normal(_) => {}
            }
        }
        Ok(())
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

    #[cfg(feature = "sqlite")]
    fn resolve_existing_db_path(
        roots: &[std::path::PathBuf],
        requested: &str,
    ) -> Result<std::path::PathBuf, String> {
        let candidate = std::path::Path::new(requested);
        if candidate.is_absolute() {
            if !candidate.exists() {
                return Err(format!("database not found: {requested}"));
            }
            if roots
                .iter()
                .any(|root| Self::safe_within(root, candidate).is_ok())
            {
                return Ok(candidate.to_path_buf());
            }
            return Err(format!(
                "absolute path '{requested}' is not within an allowed directory; \
                 register its parent via VictauriBuilder::db_search_paths"
            ));
        }

        Self::lexical_safe(candidate)?;
        for root in roots {
            let resolved = root.join(candidate);
            if resolved.exists() {
                Self::safe_within(root, &resolved)?;
                // Open the CANONICAL validated path, not the lexical join, so the DB is opened
                // at exactly the path containment approved (closes the trivial validate-lexical
                // vs open-lexical TOCTOU; a same-privilege local symlink swap between
                // canonicalize and open is the unavoidable residual, documented in security.md).
                let canonical = std::fs::canonicalize(&resolved)
                    .map_err(|e| format!("cannot resolve database path: {e}"))?;
                return Ok(canonical);
            }
        }

        let roots = roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        Err(format!(
            "database not found: {requested} (searched: {roots})"
        ))
    }

    #[cfg(feature = "sqlite")]
    fn quote_sqlite_identifier(identifier: &str) -> String {
        format!("\"{}\"", identifier.replace('"', "\"\""))
    }

    fn list_dir_recursive(
        dir: &std::path::Path,
        base: &std::path::Path,
        depth: u32,
        max_depth: u32,
        pattern: Option<&str>,
        entries: &mut Vec<serde_json::Value>,
    ) {
        if entries.len() >= MAX_DIR_ENTRIES {
            return;
        }
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read_dir.flatten() {
            if entries.len() >= MAX_DIR_ENTRIES {
                return;
            }
            let path = entry.path();
            if path.is_symlink() {
                continue;
            }
            // `is_symlink` does not cover every redirecting filesystem object
            // (notably Windows directory junctions/reparse points). Canonical
            // containment is the actual boundary before metadata or recursion.
            if Self::safe_within(base, &path).is_err() {
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

    /// Probe every window's JS bridge and report which are introspectable. A
    /// visible window that fails to respond almost always lacks the
    /// `victauri:default` capability — Tauri's permission ACL silently blocks
    /// the bridge's callback IPC, so eval/dom/animation tools see nothing. This
    /// turns that silent dead-end into an actionable, up-front diagnosis.
    async fn window_introspectability(&self) -> CallToolResult {
        let labels = self.bridge.list_window_labels();
        let states = self.bridge.get_window_states(None);
        let mut report = Vec::with_capacity(labels.len());
        let mut blind = 0usize;
        for label in &labels {
            let visible = states.iter().find(|s| &s.label == label).map(|s| s.visible);
            let introspectable = self.probe_bridge(Some(label)).await.is_ok();
            if !introspectable {
                blind += 1;
            }
            let note = if introspectable {
                "ok — Victauri JS bridge is responding".to_string()
            } else if visible == Some(true) {
                format!(
                    "NOT introspectable although the window is visible — almost certainly missing \
                     the Victauri capability. Add \"victauri:default\" to the capability file \
                     (src-tauri/capabilities/*.json) whose \"windows\" list includes \"{label}\", \
                     then rebuild. Capabilities are baked at compile time, so a rebuild is required."
                )
            } else {
                "NOT introspectable (window is hidden and/or has no bridge) — show the window to \
                 confirm, and ensure its capability includes \"victauri:default\", then rebuild."
                    .to_string()
            };
            report.push(serde_json::json!({
                "label": label,
                "visible": visible,
                "introspectable": introspectable,
                "note": note,
            }));
        }
        let hint = if blind > 0 {
            "Windows with introspectable:false have no working Victauri JS bridge — eval_js, \
             dom_snapshot, animation, find_elements, etc. cannot see them. The usual cause is a \
             missing \"victauri:default\" capability for that window: Tauri's per-window permission \
             ACL silently blocks the bridge's callback IPC. This capability is required per window, \
             not just for the main window. (Note: probing a blind window takes ~2s each.)"
        } else {
            "All windows are introspectable."
        };
        json_result(&serde_json::json!({
            "windows": report,
            "introspectable_count": labels.len().saturating_sub(blind),
            "blind_count": blind,
            "hint": hint,
        }))
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

    /// Atomically reserve a pending-eval slot under a SINGLE lock: reject if the map is
    /// already at the concurrency ceiling, otherwise insert. This makes `MAX_PENDING_EVALS`
    /// a TRUE hard ceiling — a separate check-then-insert races (concurrent callers all pass
    /// a stale check, then each inserts, blowing past the cap). On a saturated map it also
    /// fails fast (before any eval is injected) with the real "too many concurrent" cause
    /// rather than letting a probe burn its full timeout.
    async fn reserve_pending(
        &self,
        id: &str,
        tx: tokio::sync::oneshot::Sender<String>,
    ) -> Result<(), String> {
        let mut pending = self.state.pending_evals.lock().await;
        if pending.len() >= MAX_PENDING_EVALS {
            return Err(format!(
                "too many concurrent eval requests (limit: {MAX_PENDING_EVALS})"
            ));
        }
        pending.insert(id.to_string(), tx);
        Ok(())
    }

    async fn probe_bridge(&self, webview_label: Option<&str>) -> Result<(), String> {
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.reserve_pending(&id, tx).await?;
        let id_js = js_string(&id);
        let probe = format!(
            r#"(async()=>{{await window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback',{{id:{id_js},result:'"probe_ok"'}});}})();"#
        );
        if let Err(e) = self.bridge.eval_webview(webview_label, &probe) {
            self.state.pending_evals.lock().await.remove(&id);
            return Err(format!("eval injection failed: {e}"));
        }
        if let Ok(Ok(_)) = tokio::time::timeout(std::time::Duration::from_secs(2), rx).await {
            Ok(())
        } else {
            self.state.pending_evals.lock().await.remove(&id);
            let label = webview_label.unwrap_or("default");
            Err(format!(
                "bridge not responding on window '{label}' — the window may be hidden, \
                 missing the victauri capability, or the JS bridge is not loaded (e.g. the page \
                 failed to load: a dev-server connection-refused or blank error page has no JS \
                 bridge — check the window with the `screenshot` tool, which works regardless)"
            ))
        }
    }

    async fn eval_with_return_timeout(
        &self,
        code: &str,
        webview_label: Option<&str>,
        timeout: std::time::Duration,
    ) -> Result<String, String> {
        // The hard concurrency ceiling is enforced atomically at every reservation
        // (`reserve_pending`, used by both the probe and the real eval below) — NOT with a
        // separate early check, which races: concurrent callers would all pass a stale
        // `len()` read before any of them inserts. The probe is the first reservation, so a
        // saturated map is rejected fast (before any eval is injected) with the real "too
        // many concurrent" cause.

        // Wait for the JS bridge ready signal (sent on bridge init) before
        // attempting evals.  For explicitly targeted windows the probe
        // mechanism is still used because the ready signal only proves that
        // *some* webview's bridge loaded — not necessarily the targeted one.
        if !self
            .state
            .bridge_ready
            .load(std::sync::atomic::Ordering::Acquire)
        {
            let notified = self.state.bridge_notify.notified();
            if !self
                .state
                .bridge_ready
                .load(std::sync::atomic::Ordering::Acquire)
            {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), notified).await;
            }
        }

        // Reserved sentinel key for the default (unlabeled) window — cannot
        // collide with a real label.
        let label_key =
            webview_label.map_or_else(|| "\u{1}__default__".to_string(), str::to_string);

        // Liveness probe before EVERY eval — on the DEFAULT window as well as
        // labeled ones. The probe is a tiny round-trip that returns in ~ms on a
        // healthy bridge and fails fast (~2s) on a dead/hung/reloading one, turning
        // a full-timeout hang (e.g. 30s) into an immediate, clear "bridge not
        // responding" error. This was the #1 live-4DA friction: a webview that
        // reloads mid-session (HMR) made the very next tool call hang the full
        // timeout, and the DEFAULT window — the most common target — was never
        // probed at all. Probing every call (not once-cached) is what guarantees
        // *zero* 30s hangs even across repeated reloads; the healthy-path cost is a
        // single sub-millisecond localhost round-trip, negligible against the value
        // of never stalling an agent into a CDP fallback. (A saturated pending-eval
        // map is already rejected above, before this probe.)
        let prev_timed_out = self.timed_out_labels.lock().await.remove(&label_key);
        if let Err(e) = self.probe_bridge(webview_label).await {
            return Err(if prev_timed_out {
                format!(
                    "{e} (a previous eval on this window also timed out — the webview \
                     likely reloaded or the app stopped responding)"
                )
            } else {
                e
            });
        }

        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.reserve_pending(&id, tx).await?;

        // Auto-prepend `return` so bare expressions produce a value — but ONLY
        // for single expressions. Multi-statement blocks (or code containing an
        // explicit `return`) are used as-is. Prepending `return` to a statement
        // block like `foo(); return bar()` would parse as `return foo();` and
        // silently discard everything after the first statement (issue: core
        // primitive returned wrong/undefined values for "do X, then return Y").
        let code = if should_prepend_return(code) {
            format!("return {}", code.trim())
        } else {
            code.trim().to_string()
        };

        let id_js = js_string(&id);

        // Fail fast on a SYNTAX error instead of hanging for the full timeout (audit /
        // red-team "malformed eval consumes the full 30s"). The user code is inlined into
        // the script below; if it has a parse error the WHOLE script fails to parse and the
        // try/catch never runs, so the callback never fires. We cannot wrap the code in
        // `new Function`/`AsyncFunction` to surface the SyntaxError, because dynamic code
        // generation is gated by the same `unsafe-eval` CSP that blocks `eval()` — which is
        // exactly why the bridge uses an inline async-IIFE in the first place. Instead an
        // independent watchdog (which always parses) reports a parse error quickly: the
        // user-code script sets a `started` flag at its very top, so a script that fails to
        // parse never sets it. A valid-but-slow eval (e.g. a `wait_for` poll) sets `started`
        // immediately and is left to run to the real timeout — the watchdog only fires when
        // the code never began executing.
        let watchdog = format!(
            r"
            (function () {{
                window.__VIC_EVAL__ = window.__VIC_EVAL__ || {{}};
                var s = (window.__VIC_EVAL__[{id_js}] =
                    window.__VIC_EVAL__[{id_js}] || {{ started: false, done: false }});
                setTimeout(function () {{
                    if (s.started || s.done) return;
                    s.done = true;
                    try {{
                        window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', {{
                            id: {id_js},
                            result: JSON.stringify({{ __victauri_err: 'code did not begin executing within {PARSE_WATCHDOG_MS}ms — this almost always means a syntax/parse error in the submitted code (or the page main thread was blocked)' }})
                        }});
                    }} catch (e) {{}}
                    delete window.__VIC_EVAL__[{id_js}];
                }}, {PARSE_WATCHDOG_MS});
            }})();
            "
        );

        let inject = format!(
            r"
            (async () => {{
                var __s = (window.__VIC_EVAL__ && window.__VIC_EVAL__[{id_js}]) || null;
                if (__s) __s.started = true;
                try {{
                    const __result = await (async () => {{ {code} }})();
                    if (__s) {{ if (__s.done) return; __s.done = true; delete window.__VIC_EVAL__[{id_js}]; }}
                    const __type = __result === undefined ? 'undefined'
                        : __result === null ? 'null' : 'value';
                    const __val = __type === 'undefined' ? null
                        : __type === 'null' ? null : __result;
                    await window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', {{
                        id: {id_js},
                        result: JSON.stringify({{ __victauri_ok: __val, __victauri_type: __type }})
                    }});
                }} catch (e) {{
                    if (__s) {{ if (__s.done) return; __s.done = true; delete window.__VIC_EVAL__[{id_js}]; }}
                    await window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', {{
                        id: {id_js},
                        result: JSON.stringify({{ __victauri_err: String(e && e.message || e) }})
                    }});
                }}
            }})();
            "
        );

        // Inject the watchdog first so it is armed before the user code runs. Order is not
        // critical (the user-code script no-ops the watchdog state if it ran first), but
        // arming first minimises the window.
        if let Err(e) = self.bridge.eval_webview(webview_label, &watchdog) {
            self.state.pending_evals.lock().await.remove(&id);
            return Err(format!("eval injection failed: {e}"));
        }
        if let Err(e) = self.bridge.eval_webview(webview_label, &inject) {
            self.state.pending_evals.lock().await.remove(&id);
            return Err(format!("eval injection failed: {e}"));
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(raw)) => {
                self.check_bridge_version_once();
                if raw.len() > MAX_EVAL_RESULT_LEN {
                    return Err(format!(
                        "eval result too large ({} bytes, limit {MAX_EVAL_RESULT_LEN})",
                        raw.len()
                    ));
                }
                unwrap_eval_envelope(raw)
            }
            Ok(Err(_)) => Err("eval callback channel closed".to_string()),
            Err(_) => {
                self.state.pending_evals.lock().await.remove(&id);
                // Mark this window so the NEXT eval does a fast liveness probe —
                // if the bridge is gone (reloaded/crashed) the next call fails in
                // ~2s instead of blocking the full timeout again.
                self.timed_out_labels.lock().await.insert(label_key.clone());
                Err(format!(
                    "eval timed out after {}s — the code began executing but never resolved. \
                     (A syntax/parse error would have failed fast via the parse watchdog, so \
                     this is NOT a parse error.) Common causes: an unresolved promise, an \
                     infinite loop, an `await` on something that never settles, or the webview \
                     reloaded / the app stopped responding mid-eval. If the app may have \
                     navigated or crashed, retry (the next call fails fast if the bridge is \
                     gone).",
                    timeout.as_secs()
                ))
            }
        }
    }

    #[cfg(feature = "sqlite")]
    async fn run_db_health(&self, db_path: Option<&str>) -> Result<serde_json::Value, String> {
        // Roots: configured db_search_paths first, then app directories.
        let mut roots: Vec<std::path::PathBuf> = self.state.db_search_paths.clone();
        for d in [
            self.bridge.app_data_dir(),
            self.bridge.app_local_data_dir(),
            self.bridge.app_config_dir(),
        ]
        .into_iter()
        .flatten()
        {
            roots.push(d);
        }

        let path = if let Some(p) = db_path {
            Self::resolve_existing_db_path(&roots, p)?
        } else {
            // Configured db_search_paths are EXCLUSIVE when set (don't fall back to the
            // OS app dirs that hold WebView internals); WebView/engine internal stores are
            // excluded and the largest real candidate wins (audit / red-team "wrong DB").
            let select_dirs: Vec<std::path::PathBuf> = if self.state.db_search_paths.is_empty() {
                roots.clone()
            } else {
                self.state.db_search_paths.clone()
            };
            crate::database::select_app_database(&select_dirs)?
        };
        let path_str = path
            .to_str()
            .ok_or_else(|| "invalid path encoding".to_string())?
            .to_string();

        tokio::task::spawn_blocking(move || {
            let conn = rusqlite::Connection::open_with_flags(
                &path_str,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map_err(|e| format!("cannot open database: {e}"))?;
            conn.set_limit(
                rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
                MAX_DB_HEALTH_CELL_BYTES,
            );
            let started = std::time::Instant::now();
            let timed_out = Arc::new(AtomicBool::new(false));
            let timeout_marker = Arc::clone(&timed_out);
            conn.progress_handler(
                DB_HEALTH_PROGRESS_OPS,
                Some(move || {
                    let expired = started.elapsed() >= DB_HEALTH_TIMEOUT;
                    if expired {
                        timeout_marker.store(true, Ordering::Relaxed);
                    }
                    expired
                }),
            );
            // Hard wall-clock backstop for single long ops (e.g. integrity_check / per-table
            // count(*) on a huge DB) that the opcode-sampling progress handler under-counts.
            let _interrupt = crate::database::InterruptGuard::arm(&conn, DB_HEALTH_TIMEOUT);

            let journal_mode: String = conn
                .pragma_query_value(None, "journal_mode", |r| r.get(0))
                .unwrap_or_else(|_| "unknown".to_string());

            let page_count: i64 = conn
                .pragma_query_value(None, "page_count", |r| r.get(0))
                .unwrap_or(0);

            let page_size: i64 = conn
                .pragma_query_value(None, "page_size", |r| r.get(0))
                .unwrap_or(0);

            let freelist_count: i64 = conn
                .pragma_query_value(None, "freelist_count", |r| r.get(0))
                .unwrap_or(0);

            let wal_checkpoint: &str = if journal_mode == "wal" {
                "not run (read-only diagnostics)"
            } else {
                "n/a (not WAL mode)"
            };

            let integrity: String = conn
                .pragma_query_value(None, "quick_check", |r| r.get(0))
                .unwrap_or_else(|_| "failed".to_string());

            let db_size_bytes = page_count * page_size;
            let db_size_mb = db_size_bytes as f64 / (1024.0 * 1024.0);

            let mut tables = Vec::new();
            let mut table_bytes = 0usize;
            let mut tables_truncated = false;
            if let Ok(mut stmt) =
                conn.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                && let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0))
            {
                for name in rows.flatten() {
                    if tables.len() >= MAX_DB_HEALTH_TABLES
                        || table_bytes.saturating_add(name.len()) > MAX_DB_HEALTH_TABLE_BYTES
                    {
                        tables_truncated = true;
                        break;
                    }
                    table_bytes = table_bytes.saturating_add(name.len());
                    let identifier = Self::quote_sqlite_identifier(&name);
                    let count: i64 = conn
                        .query_row(&format!("SELECT count(*) FROM {identifier}"), [], |r| {
                            r.get(0)
                        })
                        .unwrap_or(0);
                    tables.push(serde_json::json!({
                        "name": name,
                        "row_count": count,
                    }));
                }
            }
            if timed_out.load(Ordering::Relaxed) {
                return Err(format!(
                    "database diagnostics timed out after {} ms",
                    DB_HEALTH_TIMEOUT.as_millis()
                ));
            }

            Ok(serde_json::json!({
                "database": path_str,
                "journal_mode": journal_mode,
                "page_count": page_count,
                "page_size": page_size,
                "db_size_mb": (db_size_mb * 100.0).round() / 100.0,
                "freelist_count": freelist_count,
                "wal_checkpoint": wal_checkpoint,
                "integrity_check": integrity,
                "tables": tables,
                "tables_truncated": tables_truncated,
            }))
        })
        .await
        .map_err(|e| format!("db health task failed: {e}"))?
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

const SERVER_INSTRUCTIONS: &str = "Victauri is a FULL-STACK inspection AND INTERVENTION tool for Tauri applications. \
It provides simultaneous access to three layers: (1) the WEBVIEW (DOM, interactions, JS eval), \
(2) the IPC LAYER (command registry, invoke commands, intercept traffic), and \
(3) the RUST BACKEND (app config, file system, SQLite databases, process memory). \
\n\nBACKEND tools (direct Rust access, no webview needed): \
'app_info' (app config, directory paths, discovered databases, process info), \
'list_app_dir' (browse app data/config/log directories), \
'read_app_file' (read files from app directories), \
'query_db' (read-only SQLite queries with auto-discovery). \
\n\nBACKEND INTROSPECTION (CDP cannot do this — Victauri-exclusive): \
'introspect' (command_timings, coverage, contract_record/check/list/clear, startup_timing, \
capabilities, db_health, plugin_state, processes, plugin_tasks, event_bus, event_bus_clear) — \
Rust-side performance profiling, IPC contract testing, command coverage analysis, startup timing, \
capability/security auditing, database diagnostics, plugin state, child process enumeration, \
task tracking, and automatic Tauri event bus monitoring. \
'fault' (inject, list, clear, clear_all) — chaos engineering: inject delays, errors, \
drops, and response corruption into Tauri commands at the Rust layer. \
'explain' (summary, last_action, diff) — cross-layer activity correlation: summarizes recent \
activity across IPC + DOM + console + network + window events into a coherent narrative. \
\n\nWEBVIEW tools: \
'interact' (click, hover, focus, scroll, select), 'input' (fill, type_text, press_key), \
'inspect' (get_styles, get_bounding_boxes, highlight, audit_accessibility, get_performance), \
'css' (inject, remove), eval_js, dom_snapshot, find_elements, screenshot. \
\n\nIPC tools: invoke_command, get_registry, detect_ghost_commands, check_ipc_integrity. \
\n\nCOMPOUND tools with an 'action' parameter: \
'window' (get_state, list, manage, resize, move_to, set_title), \
'storage' (get, set, delete, get_cookies), 'navigate' (go_to, go_back, get_history, \
set_dialog_response, get_dialog_log), 'recording' (start, stop, checkpoint, list_checkpoints, \
get_events, events_between, get_replay, export, import, replay), \
'logs' (console, network, ipc, navigation, dialogs, events, slow_ipc). \
\n\nOTHER: verify_state, wait_for (incl. 'expression'/'event' conditions to await \
async backend work to true completion), assert_semantic, resolve_command, \
app_state (app-defined backend state probes), \
get_memory_stats, get_plugin_info, get_diagnostics.";

impl ServerHandler for VictauriMcpHandler {
    fn get_info(&self) -> ServerInfo {
        // NOTE: we advertise `resources` (read) but NOT `resources.subscribe`. A real
        // server-initiated `notifications/resources/updated` push was never implemented
        // (subscribe/unsubscribe only record intent in memory; nothing emits updates), and
        // the default stateless transport has no SSE channel to push over anyway. Advertising
        // a subscribe capability we cannot honour misleads clients — read resources on demand.
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
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
        // Centralized authorization: gate on the canonical `tool.action` capability
        // resolved from the call arguments, matching the REST path in `execute_tool`.
        let args_value = serde_json::Value::Object(request.arguments.clone().unwrap_or_default());
        let capability = authz::canonical_capability(&tool_name, &args_value);
        if !self.state.privacy.is_call_allowed(&tool_name, &capability) {
            tracing::debug!(tool = %tool_name, capability = %capability, "tool call blocked by privacy config");
            return Ok(tool_disabled(&capability));
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
        // Resources bypass the tool dispatcher, so they must apply the same privacy
        // gate themselves (audit B1): a strict profile that blocks log/window reads
        // as tools must not be able to read the same data via a resource.
        if let Some(cap) = resource_required_capability(uri.as_str())
            && !self.state.privacy.is_tool_enabled(cap)
        {
            return Err(ErrorData::invalid_request(
                format!("resource {uri} is not permitted by the current privacy configuration"),
                None,
            ));
        }
        let json = match uri.as_str() {
            RESOURCE_URI_IPC_LOG => {
                // Use the body-free, capped projection — NOT the full body-carrying
                // getIpcLog(). On a busy app the full log blows the eval result cap, the
                // eval fails, and we silently fall back to the Rust event_log (which is
                // itself default-window-drained) — serving a subset that looks complete.
                // trimmed_log_js bounds entries + truncates oversized fields so the
                // resource stays correct under load. (Matches the `logs ipc` tool.)
                let code = trimmed_log_js("window.__VICTAURI__?.getIpcLog()", DEFAULT_LOG_LIMIT);
                if let Ok(json) = self.eval_with_return(&code, None).await {
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
        // Same privacy gate as read_resource (audit B1) — don't let a blocked
        // resource be subscribed to for push updates.
        if let Some(cap) = resource_required_capability(uri.as_str())
            && !self.state.privacy.is_tool_enabled(cap)
        {
            return Err(ErrorData::invalid_request(
                format!("resource {uri} is not permitted by the current privacy configuration"),
                None,
            ));
        }
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

/// Build a JS expression that takes an array of log entries (`source_expr`),
/// keeps at most `limit` of the most recent, and truncates any per-entry field
/// larger than [`MAX_LOG_FIELD_BYTES`]. This keeps IPC/network log results under
/// the eval size cap on busy apps where individual entries carry large bodies.
///
/// The returned code is a complete `return (...)` statement.
fn trimmed_log_js(source_expr: &str, limit: usize) -> String {
    let mb = MAX_LOG_FIELD_BYTES;
    format!(
        r"return (function() {{
            var MB = {mb};
            function trimField(v) {{
                if (typeof v === 'string') {{
                    return v.length > MB ? (v.slice(0, MB) + '…[+' + (v.length - MB) + ' bytes truncated]') : v;
                }}
                if (v && typeof v === 'object') {{
                    var s; try {{ s = JSON.stringify(v); }} catch (e) {{ s = ''; }}
                    if (s.length > MB) {{ return '[truncated ' + s.length + ' bytes]'; }}
                }}
                return v;
            }}
            function trimEntry(e) {{
                if (e == null || typeof e !== 'object') return e;
                var out = Array.isArray(e) ? [] : {{}};
                for (var k in e) {{ if (Object.prototype.hasOwnProperty.call(e, k)) out[k] = trimField(e[k]); }}
                return out;
            }}
            var arr = {source_expr} || [];
            if (arr.length > {limit}) arr = arr.slice(-{limit});
            return arr.map(trimEntry);
        }})()"
    )
}

/// Unwrap the `{"__victauri_ok": <val>, "__victauri_type": <t>}` (or
/// `{"__victauri_err": <msg>}`) envelope produced by the eval bridge into the
/// value/error string returned to callers.
///
/// Parsing uses `serde_json`'s default recursion limit (it is intentionally NOT
/// disabled — an unbounded recursive parse of a pathologically deep result
/// overflows the worker thread stack and crashes the host). When the parse
/// fails because the value is too deeply nested, the envelope is stripped by
/// string slicing (no recursion) so the actual value is still returned rather
/// than leaking the raw envelope string.
fn unwrap_eval_envelope(raw: String) -> Result<String, String> {
    if let Ok(envelope) = serde_json::from_str::<serde_json::Value>(&raw) {
        if let Some(err) = envelope.get("__victauri_err") {
            return Err(format!(
                "JavaScript error: {}",
                err.as_str().unwrap_or("unknown error")
            ));
        }
        if envelope.get("__victauri_ok").is_some() {
            let js_type = envelope
                .get("__victauri_type")
                .and_then(|t| t.as_str())
                .unwrap_or("value");
            return match js_type {
                "undefined" => Ok("undefined".to_string()),
                "null" => Ok("null".to_string()),
                _ => Ok(serde_json::to_string(&envelope["__victauri_ok"])
                    .unwrap_or_else(|_| "null".to_string())),
            };
        }
    }
    // Fallback for results too deeply nested for the recursion-limited parser.
    if let Some(after) = raw.strip_prefix(r#"{"__victauri_ok":"#)
        && let Some(idx) = after.rfind(r#","__victauri_type":"#)
    {
        return Ok(after[..idx].to_string());
    }
    if let Some(after) = raw.strip_prefix(r#"{"__victauri_err":"#) {
        let msg = after.trim_end_matches('}').trim_matches('"');
        return Err(format!("JavaScript error: {msg}"));
    }
    Ok(raw)
}

/// Statement keywords where a leading `return` would be a syntax error.
const STMT_STARTS: &[&str] = &[
    "return ",
    "return;",
    "return\n",
    "return\t",
    "if ",
    "if(",
    "for ",
    "for(",
    "while ",
    "while(",
    "switch ",
    "switch(",
    "try ",
    "try{",
    "const ",
    "let ",
    "var ",
    "function ",
    "function(",
    "function*",
    "class ",
    "throw ",
    "do ",
    "do{",
    "{",
    "async function",
    "debugger",
];

/// String/template/comment scan state for [`should_prepend_return`].
#[derive(PartialEq, Clone, Copy)]
enum ScanState {
    Code,
    SingleQuote,
    DoubleQuote,
    Template,
}

/// Decide whether to wrap `code` with a leading `return`.
///
/// Only a single bare expression should get `return` prepended. Code that is a
/// multi-statement block, contains an explicit top-level `return`, or starts
/// with a statement keyword is used as-is — prepending `return` to such code
/// would execute only the first statement and silently discard the rest.
///
/// The scan is string/template/comment-aware and only treats a `;` or an
/// explicit `return` token as significant when it occurs at bracket depth 0
/// outside of any string, template literal, or comment.
fn should_prepend_return(code: &str) -> bool {
    use ScanState::{Code, DoubleQuote, SingleQuote, Template};

    let code = code.trim();
    if code.is_empty() {
        return false;
    }

    if STMT_STARTS.iter().any(|k| code.starts_with(k)) {
        return false;
    }

    let bytes = code.as_bytes();
    let mut i = 0;
    let mut depth: i32 = 0;
    let mut state = ScanState::Code;

    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    // Is there a top-level `return` token starting at byte `i` (word-bounded)?
    let is_return_token = |i: usize| -> bool {
        let prev_ok = i == 0 || !is_ident(bytes[i - 1]);
        prev_ok
            && code[i..].starts_with("return")
            && bytes.get(i + 6).copied().is_none_or(|b| !is_ident(b))
    };

    while i < bytes.len() {
        let c = bytes[i];
        match state {
            Code => match c {
                b'\'' => state = SingleQuote,
                b'"' => state = DoubleQuote,
                b'`' => state = Template,
                b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
                b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                    i += 2;
                    while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                        i += 1;
                    }
                    i += 2;
                    continue;
                }
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                // A top-level `;` with more code after it == multi-statement.
                b';' if depth <= 0 && !code[i + 1..].trim().is_empty() => return false,
                // An explicit top-level `return` token means the code already returns.
                b'r' if depth <= 0 && is_return_token(i) => return false,
                _ => {}
            },
            SingleQuote => {
                if c == b'\\' {
                    i += 1;
                } else if c == b'\'' {
                    state = Code;
                }
            }
            DoubleQuote => {
                if c == b'\\' {
                    i += 1;
                } else if c == b'"' {
                    state = Code;
                }
            }
            Template => {
                if c == b'\\' {
                    i += 1;
                } else if c == b'`' {
                    state = Code;
                }
            }
        }
        i += 1;
    }

    true
}

#[cfg(test)]
mod prop_tests {
    //! Property-based tests for the eval auto-return heuristic — the code that
    //! caused the worst bug in the system (silent corruption of multi-statement
    //! eval) and has bitten twice. These generate many JS-ish snippets and
    //! assert the invariants that keep eval correct.
    use super::should_prepend_return;
    use proptest::prelude::*;

    /// A small set of non-keyword identifier-ish expressions.
    fn ident() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("a".to_string()),
            Just("x".to_string()),
            Just("foo".to_string()),
            Just("window.x".to_string()),
            Just("document.title".to_string()),
            Just("obj.prop".to_string()),
            Just("arr[0]".to_string()),
            Just("localStorage".to_string()),
        ]
    }

    /// A single bare expression: never starts with a statement keyword, has no
    /// top-level `;`, and contains no `return`.
    fn bare_expr() -> impl Strategy<Value = String> {
        prop_oneof![
            ident(),
            (ident(), ident()).prop_map(|(a, b)| format!("{a} + {b}")),
            (ident(), ident()).prop_map(|(a, b)| format!("{a}({b})")),
            ident().prop_map(|a| format!("{a}.length")),
            any::<u16>().prop_map(|n| n.to_string()),
        ]
    }

    proptest! {
        /// Must never panic or hang on ANY input — including malformed code,
        /// unbalanced quotes, and arbitrary unicode (the scanner indexes bytes).
        #[test]
        fn never_panics_on_arbitrary_input(s in ".{0,256}") {
            let _ = should_prepend_return(&s);
        }

        /// A single bare expression is safe to wrap with `return` → true.
        #[test]
        fn bare_expressions_are_prepended(e in bare_expr()) {
            prop_assert!(should_prepend_return(&e), "bare expr not prepended: {e:?}");
        }

        /// THE critical bug class: `<expr>; return <expr>` must NOT be prepended
        /// (else `return <expr>;` runs and the rest is silently discarded).
        #[test]
        fn semicolon_multistatement_with_return_never_prepended(
            setup in bare_expr(), ret in bare_expr()
        ) {
            let code = format!("{setup}; return {ret}");
            prop_assert!(!should_prepend_return(&code), "would corrupt: {code:?}");
        }

        /// Newline-separated (ASI) explicit return must also be left as-is.
        #[test]
        fn newline_explicit_return_never_prepended(pre in bare_expr(), ret in bare_expr()) {
            let code = format!("{pre}\nreturn {ret}");
            prop_assert!(!should_prepend_return(&code), "explicit return prepended: {code:?}");
        }

        /// `;` or the word `return` INSIDE a string literal must not trigger a
        /// false multi-statement split — a bare string is one expression.
        #[test]
        fn semicolons_and_return_inside_strings_are_ignored(inner in "[a-z0-9;= ]{0,24}") {
            // `inner` never contains a quote, so the literal is well-formed.
            let code = format!("'do;not;split return {inner}'");
            prop_assert!(should_prepend_return(&code), "string literal mis-split: {code:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "sqlite")]
    #[test]
    fn database_path_resolution_rejects_lexical_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("allowed");
        std::fs::create_dir(&root).unwrap();
        std::fs::File::create(dir.path().join("outside.db")).unwrap();

        let err =
            VictauriMcpHandler::resolve_existing_db_path(&[root], "../outside.db").unwrap_err();
        assert!(err.contains("path traversal"), "unexpected error: {err}");
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn database_path_resolution_accepts_contained_nested_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("allowed");
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let db = nested.join("app.db");
        std::fs::File::create(&db).unwrap();

        let resolved =
            VictauriMcpHandler::resolve_existing_db_path(&[root], "nested/app.db").unwrap();
        // Resolution returns the CANONICAL validated path (opened == validated, closing the
        // lexical-vs-canonical TOCTOU), so compare against the canonical form of the target.
        assert_eq!(resolved, std::fs::canonicalize(&db).unwrap());
    }

    #[cfg(all(feature = "sqlite", unix))]
    #[test]
    fn database_path_resolution_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("allowed");
        std::fs::create_dir(&root).unwrap();
        let outside = dir.path().join("outside.db");
        std::fs::File::create(&outside).unwrap();
        symlink(&outside, root.join("linked.db")).unwrap();

        let err = VictauriMcpHandler::resolve_existing_db_path(&[root], "linked.db").unwrap_err();
        assert!(err.contains("path traversal"), "unexpected error: {err}");
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_identifier_quoting_handles_hostile_table_names() {
        let file = tempfile::NamedTempFile::with_suffix(".sqlite").unwrap();
        let conn = rusqlite::Connection::open(file.path()).unwrap();
        let name = "odd\"] table";
        let identifier = VictauriMcpHandler::quote_sqlite_identifier(name);
        conn.execute_batch(&format!(
            "CREATE TABLE {identifier} (id INTEGER); INSERT INTO {identifier} VALUES (1);"
        ))
        .unwrap();
        let count: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {identifier}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn env_filter_drops_secrets_keeps_safe() {
        // Safe, non-secret vars pass.
        assert!(is_safe_env_key("HOME"));
        assert!(is_safe_env_key("LANG"));
        assert!(is_safe_env_key("TAURI_ENV_PLATFORM"));
        assert!(is_safe_env_key("VICTAURI_PORT"));
        // Secret-looking vars are dropped even under a safe prefix (audit #5).
        assert!(!is_safe_env_key("TAURI_SIGNING_PRIVATE_KEY"));
        assert!(!is_safe_env_key("TAURI_SIGNING_PRIVATE_KEY_PASSWORD"));
        assert!(!is_safe_env_key("VICTAURI_AUTH_TOKEN"));
        assert!(!is_safe_env_key("VICTAURI_API_KEY"));
        // Unknown prefixes are dropped regardless.
        assert!(!is_safe_env_key("AWS_SECRET_ACCESS_KEY"));
        assert!(!is_safe_env_key("RANDOM_VAR"));
        // The broad TAURI_ namespace is no longer allowed — only TAURI_ENV_ — so
        // app-custom TAURI_ secrets are dropped even without a denylist hit.
        assert!(!is_safe_env_key("TAURI_CUSTOM_THING"));
        // Adversarial leaks closed (audit #5 follow-up): connection strings,
        // passphrases, PATs, JWTs, etc. under an allowed prefix.
        assert!(!is_safe_env_key("VICTAURI_DB_DSN"));
        assert!(!is_safe_env_key("VICTAURI_SIGNING_PASSPHRASE"));
        assert!(!is_safe_env_key("VICTAURI_GH_PAT"));
        assert!(!is_safe_env_key("VICTAURI_JWT"));
        assert!(!is_safe_env_key("VICTAURI_SESSION_ID"));
    }

    #[test]
    fn prepend_return_bare_expressions() {
        assert!(should_prepend_return("document.title"));
        assert!(should_prepend_return("5 + 5"));
        assert!(should_prepend_return("\"justexpr\""));
        assert!(should_prepend_return("await fetch('/x')"));
        assert!(should_prepend_return(
            "document.querySelectorAll('a').length"
        ));
        assert!(should_prepend_return("x ? a : b"));
        // Single trailing semicolon on a bare expression is still an expression.
        assert!(should_prepend_return("document.title;"));
        // Semicolons inside strings must not be treated as boundaries.
        assert!(should_prepend_return("'a;b;c'"));
        assert!(should_prepend_return("\"x;y\".length"));
        // IIFE workaround: the `;` lives inside the arrow body (depth > 0).
        assert!(should_prepend_return("(()=>{window.x=5; return 'ok'})()"));
    }

    #[test]
    fn no_prepend_for_statement_blocks() {
        // The original silent-corruption cases.
        assert!(!should_prepend_return(
            "localStorage.setItem('k','v'); return localStorage.getItem('k')"
        ));
        assert!(!should_prepend_return(
            "window.scrollTo(0,50); return window.scrollY"
        ));
        assert!(!should_prepend_return("console.log('x'); return 123"));
        assert!(!should_prepend_return("window.__z=7; return 'ok'"));
        // Explicit return without a preceding semicolon (newline-separated).
        assert!(!should_prepend_return("window.x = 5\nreturn window.x"));
    }

    #[test]
    fn no_prepend_for_statement_keywords() {
        assert!(!should_prepend_return("return 42"));
        assert!(!should_prepend_return("const x = 1; return x"));
        assert!(!should_prepend_return("let y = 2"));
        assert!(!should_prepend_return("var z = 3"));
        assert!(!should_prepend_return("if (x) { return 1 }"));
        assert!(!should_prepend_return("for (const x of y) doThing(x)"));
        assert!(!should_prepend_return("throw new Error('x')"));
        assert!(!should_prepend_return("function f(){}"));
        assert!(!should_prepend_return("{ a: 1 }")); // object-literal-as-block ambiguity → as-is
    }

    #[test]
    fn empty_code_no_prepend() {
        assert!(!should_prepend_return(""));
        assert!(!should_prepend_return("   "));
    }

    #[test]
    fn envelope_unwrap_value() {
        assert_eq!(
            unwrap_eval_envelope(r#"{"__victauri_ok":"4DA","__victauri_type":"value"}"#.into()),
            Ok("\"4DA\"".to_string())
        );
        assert_eq!(
            unwrap_eval_envelope(r#"{"__victauri_ok":42,"__victauri_type":"value"}"#.into()),
            Ok("42".to_string())
        );
    }

    #[test]
    fn envelope_unwrap_undefined_null() {
        assert_eq!(
            unwrap_eval_envelope(r#"{"__victauri_ok":null,"__victauri_type":"undefined"}"#.into()),
            Ok("undefined".to_string())
        );
        assert_eq!(
            unwrap_eval_envelope(r#"{"__victauri_ok":null,"__victauri_type":"null"}"#.into()),
            Ok("null".to_string())
        );
    }

    #[test]
    fn envelope_unwrap_error() {
        let r = unwrap_eval_envelope(r#"{"__victauri_err":"boom"}"#.into());
        assert!(r.unwrap_err().contains("boom"));
    }

    #[test]
    fn envelope_unwrap_deeply_nested_does_not_leak() {
        // Build an envelope whose value is nested far deeper than serde_json's
        // default recursion limit (128). The full parse fails, so the slice
        // fallback must return the value — NOT the raw `__victauri_ok` envelope.
        let mut value = String::from("0");
        for _ in 0..300 {
            value = format!("{{\"n\":{value}}}");
        }
        let raw = format!(r#"{{"__victauri_ok":{value},"__victauri_type":"value"}}"#);
        let out = unwrap_eval_envelope(raw).unwrap();
        assert!(
            out.starts_with(r#"{"n":"#),
            "deep value should be unwrapped, got: {}",
            &out[..out.len().min(40)]
        );
        assert!(
            !out.contains("__victauri_ok"),
            "envelope must not leak into the result"
        );
    }

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

/// Dispatch-level authorization tests.
///
/// These exercise the REAL `execute_tool` dispatch path (not just the privacy
/// string matrix) to prove that blocked tools/actions actually return
/// `tool_disabled` and never reach their handler. This is the negative security
/// suite the audit required (Gate #5): the prior tests validated
/// `is_tool_enabled(...)` in isolation, which let structural dispatch bypasses
/// pass undetected.
#[cfg(test)]
mod authz_dispatch_tests {
    use super::*;
    use crate::bridge::WebviewBridge;
    use crate::privacy::PrivacyConfig;
    use std::collections::{HashMap, HashSet};
    use victauri_core::{CommandRegistry, EventLog, EventRecorder, WindowState};

    /// A bridge whose eval always fails immediately, so an *allowed* action that
    /// reaches the bridge returns a non-privacy error fast (no 30s hang), while a
    /// *blocked* action is rejected by dispatch before the bridge is ever touched.
    struct RejectingBridge;

    impl WebviewBridge for RejectingBridge {
        fn eval_webview(&self, _label: Option<&str>, _script: &str) -> Result<(), String> {
            Err("eval rejected in authz dispatch test".to_string())
        }
        fn get_window_states(&self, _label: Option<&str>) -> Vec<WindowState> {
            Vec::new()
        }
        fn list_window_labels(&self) -> Vec<String> {
            Vec::new()
        }
        fn get_native_handle(&self, _label: Option<&str>) -> Result<isize, String> {
            Err("no handle".to_string())
        }
        fn manage_window(&self, _label: Option<&str>, _action: &str) -> Result<String, String> {
            Err("no window".to_string())
        }
        fn resize_window(&self, _l: Option<&str>, _w: u32, _h: u32) -> Result<(), String> {
            Ok(())
        }
        fn move_window(&self, _l: Option<&str>, _x: i32, _y: i32) -> Result<(), String> {
            Ok(())
        }
        fn set_window_title(&self, _l: Option<&str>, _t: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn state_with(privacy: PrivacyConfig) -> Arc<VictauriState> {
        Arc::new(VictauriState {
            event_log: EventLog::new(1000),
            registry: CommandRegistry::new(),
            port: std::sync::atomic::AtomicU16::new(0),
            pending_evals: Arc::new(Mutex::new(HashMap::new())),
            recorder: EventRecorder::new(1000),
            privacy,
            eval_timeout: std::time::Duration::from_millis(100),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            started_at: std::time::Instant::now(),
            tool_invocations: std::sync::atomic::AtomicU64::new(0),
            allow_file_navigation: false,
            command_timings: crate::introspection::CommandTimings::new(),
            fault_registry: crate::introspection::FaultRegistry::new(),
            contract_store: crate::introspection::ContractStore::new(),
            startup_timeline: crate::introspection::StartupTimeline::new(),
            event_bus: crate::introspection::EventBusMonitor::default(),
            task_tracker: crate::introspection::TaskTracker::new(),
            bridge_ready: std::sync::atomic::AtomicBool::new(true),
            bridge_notify: tokio::sync::Notify::new(),
            db_search_paths: Vec::new(),
            screencast: Arc::new(crate::screencast::Screencast::default()),
            probes: crate::introspection::AppStateProbes::default(),
        })
    }

    fn handler(privacy: PrivacyConfig) -> VictauriMcpHandler {
        VictauriMcpHandler::new(state_with(privacy), Arc::new(RejectingBridge))
    }

    /// True iff the result is a privacy/authorization block (vs any other error).
    fn is_privacy_blocked(r: &CallToolResult) -> bool {
        r.is_error == Some(true)
            && r.content.iter().any(|c| {
                matches!(&c.raw, RawContent::Text(t)
                    if t.text.contains("disabled by privacy configuration"))
            })
    }

    async fn call(h: &VictauriMcpHandler, tool: &str, args: serde_json::Value) -> CallToolResult {
        match h.execute_tool(tool, args).await {
            Ok(r) => r,
            Err(_) => panic!("dispatch returned a transport error (arg parse failure)"),
        }
    }

    // ── Observe profile: every mutation/eval/compound-action must be blocked ──

    #[tokio::test]
    async fn observe_blocks_mutations_and_eval_through_dispatch() {
        let h = handler(crate::privacy::observe_privacy_config());
        let blocked: &[(&str, serde_json::Value)] = &[
            ("eval_js", serde_json::json!({"code": "1"})),
            (
                "wait_for",
                serde_json::json!({"condition": "expression", "value": "true"}),
            ),
            ("screenshot", serde_json::json!({})),
            ("invoke_command", serde_json::json!({"command": "greet"})),
            ("verify_state", serde_json::json!({"frontend_expr": "1"})),
            (
                "assert_semantic",
                serde_json::json!({"expression": "1", "condition": "truthy"}),
            ),
            (
                "interact",
                serde_json::json!({"action": "click", "ref_id": "e1"}),
            ),
            (
                "input",
                serde_json::json!({"action": "fill", "ref_id": "e1", "value": "x"}),
            ),
            (
                "storage",
                serde_json::json!({"action": "set", "key": "k", "value": "v"}),
            ),
            (
                "storage",
                serde_json::json!({"action": "delete", "key": "k"}),
            ),
            (
                "window",
                serde_json::json!({"action": "manage", "manage_action": "close"}),
            ),
            (
                "window",
                serde_json::json!({"action": "set_title", "title": "x"}),
            ),
            (
                "navigate",
                serde_json::json!({"action": "go_to", "url": "https://e.com"}),
            ),
            (
                "css",
                serde_json::json!({"action": "inject", "css": "body{}"}),
            ),
            ("route", serde_json::json!({"action": "clear_all"})),
            ("recording", serde_json::json!({"action": "start"})),
            ("recording", serde_json::json!({"action": "replay"})),
            ("logs", serde_json::json!({"action": "clear"})),
            (
                "fault",
                serde_json::json!({"action": "inject", "command": "x", "fault_type": "error"}),
            ),
            (
                "introspect",
                serde_json::json!({"action": "command_timings"}),
            ),
        ];
        for (tool, args) in blocked {
            let r = call(&h, tool, args.clone()).await;
            assert!(
                is_privacy_blocked(&r),
                "Observe must block {tool} {args} at dispatch, got: {:?}",
                r.content
            );
        }
    }

    #[tokio::test]
    async fn observe_allows_read_only_through_dispatch() {
        let h = handler(crate::privacy::observe_privacy_config());
        // These reads must NOT be privacy-blocked (they may fail for other reasons
        // against the rejecting bridge, but never with a privacy block).
        let allowed: &[(&str, serde_json::Value)] = &[
            ("get_registry", serde_json::json!({})),
            ("get_memory_stats", serde_json::json!({})),
            ("window", serde_json::json!({"action": "list"})),
            ("logs", serde_json::json!({"action": "ipc"})),
            (
                "inspect",
                serde_json::json!({"action": "get_styles", "ref_id": "e1"}),
            ),
        ];
        for (tool, args) in allowed {
            let r = call(&h, tool, args.clone()).await;
            assert!(
                !is_privacy_blocked(&r),
                "Observe must allow {tool} {args} at dispatch (blocked unexpectedly)"
            );
        }
    }

    // ── Test profile: interactions allowed, eval/replay/route blocked ─────────

    #[tokio::test]
    async fn test_profile_dispatch_boundaries() {
        let h = handler(crate::privacy::test_privacy_config());
        // Allowed in Test:
        for (tool, args) in [
            (
                "interact",
                serde_json::json!({"action": "click", "ref_id": "e1"}),
            ),
            (
                "input",
                serde_json::json!({"action": "fill", "ref_id": "e1", "value": "x"}),
            ),
            (
                "storage",
                serde_json::json!({"action": "set", "key": "k", "value": "v"}),
            ),
            ("navigate", serde_json::json!({"action": "go_back"})),
            ("recording", serde_json::json!({"action": "start"})),
            ("logs", serde_json::json!({"action": "clear"})),
        ] {
            let r = call(&h, tool, args.clone()).await;
            assert!(!is_privacy_blocked(&r), "Test must allow {tool} {args}");
        }
        // Blocked in Test (arbitrary eval, navigation mutation, replay, FullControl tools):
        for (tool, args) in [
            ("eval_js", serde_json::json!({"code": "1"})),
            (
                "wait_for",
                serde_json::json!({"condition": "expression", "value": "true"}),
            ),
            ("verify_state", serde_json::json!({"frontend_expr": "1"})),
            (
                "navigate",
                serde_json::json!({"action": "go_to", "url": "https://e.com"}),
            ),
            ("recording", serde_json::json!({"action": "replay"})),
            (
                "route",
                serde_json::json!({"action": "add", "pattern": "x"}),
            ),
            ("css", serde_json::json!({"action": "inject", "css": "x"})),
            (
                "window",
                serde_json::json!({"action": "set_title", "title": "x"}),
            ),
        ] {
            let r = call(&h, tool, args.clone()).await;
            assert!(is_privacy_blocked(&r), "Test must block {tool} {args}");
        }
    }

    // ── disabled_tools: bare-name disable covers all of a compound tool's
    //    actions, and per-action disable is honored even when the handler
    //    historically did not check it (the route.clear bypass). ──────────────

    #[tokio::test]
    async fn disabling_bare_compound_tool_blocks_all_actions() {
        let cfg = PrivacyConfig {
            disabled_tools: HashSet::from(["recording".to_string()]),
            ..Default::default()
        }; // FullControl with the whole `recording` tool disabled
        let h = handler(cfg);
        for action in ["start", "stop", "replay", "import", "export"] {
            let r = call(&h, "recording", serde_json::json!({"action": action})).await;
            assert!(
                is_privacy_blocked(&r),
                "disabling bare `recording` must block recording.{action}"
            );
        }
    }

    #[tokio::test]
    async fn disabling_specific_action_is_honored_at_dispatch() {
        // The historical bypass: `route.clear`'s handler had no per-action check,
        // so a `disabled_tools` entry for it was silently ignored. The central
        // gate now enforces it.
        let cfg = PrivacyConfig {
            disabled_tools: HashSet::from([
                "route.clear".to_string(),
                "route.clear_all".to_string(),
            ]),
            ..Default::default()
        }; // FullControl: everything else allowed
        let h = handler(cfg);

        let blocked = call(&h, "route", serde_json::json!({"action": "clear", "id": 1})).await;
        assert!(is_privacy_blocked(&blocked), "route.clear must be blocked");
        let blocked_all = call(&h, "route", serde_json::json!({"action": "clear_all"})).await;
        assert!(
            is_privacy_blocked(&blocked_all),
            "route.clear_all must be blocked"
        );

        // A sibling action the operator did NOT disable is still reachable.
        let allowed = call(&h, "route", serde_json::json!({"action": "list"})).await;
        assert!(
            !is_privacy_blocked(&allowed),
            "route.list must remain allowed"
        );
    }

    // Command-policy enforcement on invoke paths (A1/A2) and resource gating (B1)
    // are covered with side-effect detection (a bridge that records actual invokes)
    // in the `command_policy_dispatch_tests` module below — that proves the blocked
    // command never reaches the bridge, not merely that an error string is returned.

    #[tokio::test]
    async fn full_control_allows_everything_at_dispatch() {
        let h = handler(PrivacyConfig::default());
        for (tool, args) in [
            ("recording", serde_json::json!({"action": "replay"})),
            ("route", serde_json::json!({"action": "clear_all"})),
            ("eval_js", serde_json::json!({"code": "1"})),
            ("fault", serde_json::json!({"action": "list"})),
        ] {
            let r = call(&h, tool, args.clone()).await;
            assert!(
                !is_privacy_blocked(&r),
                "FullControl must allow {tool} {args}"
            );
        }
    }
}

/// Command-policy enforcement on EVERY command-invoking path (audit #30/#31, triage A1/A2).
///
/// The prior privacy suite validated the permission-string matrix — `is_tool_enabled("x")`
/// in isolation — which let structural dispatch bypasses pass undetected (the audit's
/// central criticism: "tests validate the STRING MATRIX, not actual dispatch behavior").
///
/// These tests instead drive the REAL dispatcher with a bridge that records every script
/// handed to `eval_webview`, and assert the dangerous **side effect** — the
/// `__TAURI_INTERNALS__.invoke(<command>)` script — is NEVER emitted when the command is on
/// the operator's blocklist, on each path that invokes commands OUTSIDE `invoke_command`:
/// `recording.replay`, `recording.import` + `replay`, `introspect.contract_record`, and
/// `introspect.contract_check`. Each has a positive control proving an *allowed* command IS
/// invoked (so a blanket-block can't make the negative test pass vacuously).
#[cfg(test)]
mod command_policy_dispatch_tests {
    use super::*;
    use crate::bridge::WebviewBridge;
    use crate::privacy::PrivacyConfig;
    use serde_json::json;
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex as StdMutex;
    use victauri_core::{
        AppEvent, CommandRegistry, EventLog, EventRecorder, IpcCall, IpcResult, RecordedEvent,
        RecordedSession, WindowState,
    };

    /// A bridge that RECORDS every script passed to `eval_webview` (so a test can assert a
    /// blocklisted command's invoke was never emitted) then fails the eval fast — an allowed
    /// command is observably *attempted* without hanging on a callback that never arrives.
    ///
    /// When constructed via [`RecordingBridge::answering`] it also resolves the pre-eval
    /// liveness probe, simulating a healthy webview so an ALLOWED command's invoke actually
    /// reaches the bridge. Default-constructed bridges leave the probe unanswered — which is
    /// fine for negative tests, since a blocked command is rejected at the privacy gate
    /// *before* any eval (and thus never probes).
    #[derive(Clone, Default)]
    struct RecordingBridge {
        scripts: Arc<StdMutex<Vec<String>>>,
        pending_evals: Option<crate::PendingCallbacks>,
    }

    /// Extract the 36-char eval id from a probe script of the form `…id:"<uuid>"…`.
    fn extract_probe_id(script: &str) -> Option<String> {
        let start = script.find("id:\"")? + 4;
        script.get(start..start + 36).map(str::to_string)
    }

    impl RecordingBridge {
        /// A recording bridge that answers the liveness probe with the state's pending-evals
        /// map, so a permitted command's eval proceeds past the probe and is observably
        /// injected.
        fn answering(pending_evals: crate::PendingCallbacks) -> Self {
            Self {
                scripts: Arc::default(),
                pending_evals: Some(pending_evals),
            }
        }

        /// True iff any recorded eval script invoked `command` via the Tauri IPC bridge.
        fn invoked(&self, command: &str) -> bool {
            let needle = format!("invoke({}", js_string(command));
            self.scripts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .any(|s| s.contains(&needle))
        }
    }

    impl WebviewBridge for RecordingBridge {
        fn eval_webview(&self, _label: Option<&str>, script: &str) -> Result<(), String> {
            self.scripts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(script.to_string());
            // If wired with a pending-evals map, answer the pre-eval liveness probe
            // (simulating a healthy webview) so the real eval proceeds past it. The
            // real eval is still left unanswered, so it times out fast at the 100ms
            // test `eval_timeout` — we only care WHICH scripts reached the bridge,
            // never the eval's return value.
            if let Some(pending) = &self.pending_evals
                && script.contains("probe_ok")
                && let Some(id) = extract_probe_id(script)
            {
                let pending = pending.clone();
                std::thread::spawn(move || {
                    let mut map = pending.blocking_lock();
                    if let Some(tx) = map.remove(&id) {
                        let _ = tx.send("\"probe_ok\"".to_string());
                    }
                });
            }
            // Return Ok so `eval_with_return` injects BOTH its watchdog and the
            // user-code script (it bails on the first Err).
            Ok(())
        }
        fn get_window_states(&self, _l: Option<&str>) -> Vec<WindowState> {
            Vec::new()
        }
        fn list_window_labels(&self) -> Vec<String> {
            Vec::new()
        }
        fn get_native_handle(&self, _l: Option<&str>) -> Result<isize, String> {
            Err("no handle".to_string())
        }
        fn manage_window(&self, _l: Option<&str>, _a: &str) -> Result<String, String> {
            Err("no window".to_string())
        }
        fn resize_window(&self, _l: Option<&str>, _w: u32, _h: u32) -> Result<(), String> {
            Ok(())
        }
        fn move_window(&self, _l: Option<&str>, _x: i32, _y: i32) -> Result<(), String> {
            Ok(())
        }
        fn set_window_title(&self, _l: Option<&str>, _t: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn state_with(privacy: PrivacyConfig) -> Arc<VictauriState> {
        Arc::new(VictauriState {
            event_log: EventLog::new(1000),
            registry: CommandRegistry::new(),
            port: std::sync::atomic::AtomicU16::new(0),
            pending_evals: Arc::new(Mutex::new(HashMap::new())),
            recorder: EventRecorder::new(1000),
            privacy,
            eval_timeout: std::time::Duration::from_millis(100),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            started_at: std::time::Instant::now(),
            tool_invocations: std::sync::atomic::AtomicU64::new(0),
            allow_file_navigation: false,
            command_timings: crate::introspection::CommandTimings::new(),
            fault_registry: crate::introspection::FaultRegistry::new(),
            contract_store: crate::introspection::ContractStore::new(),
            startup_timeline: crate::introspection::StartupTimeline::new(),
            event_bus: crate::introspection::EventBusMonitor::default(),
            task_tracker: crate::introspection::TaskTracker::new(),
            bridge_ready: std::sync::atomic::AtomicBool::new(true),
            bridge_notify: tokio::sync::Notify::new(),
            db_search_paths: Vec::new(),
            screencast: Arc::new(crate::screencast::Screencast::default()),
            probes: crate::introspection::AppStateProbes::default(),
        })
    }

    // FullControl, except the named commands are blocklisted — exactly the scenario
    // the audit flagged: an operator who trusts `command_blocklist` to stop a
    // dangerous command.
    fn blocking(cmds: &[&str]) -> PrivacyConfig {
        PrivacyConfig {
            command_blocklist: cmds.iter().map(|s| (*s).to_string()).collect(),
            ..Default::default()
        }
    }

    fn ipc_event(command: &str) -> AppEvent {
        AppEvent::Ipc(IpcCall {
            id: format!("c-{command}"),
            command: command.to_string(),
            timestamp: chrono::Utc::now(),
            duration_ms: Some(1),
            result: IpcResult::Ok(json!(true)),
            arg_size_bytes: 0,
            webview_label: "main".to_string(),
        })
    }

    fn result_text(r: &CallToolResult) -> String {
        r.content
            .iter()
            .filter_map(|c| match &c.raw {
                RawContent::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn call(h: &VictauriMcpHandler, tool: &str, args: serde_json::Value) -> CallToolResult {
        match h.execute_tool(tool, args).await {
            Ok(r) => r,
            Err(_) => panic!("dispatch returned a transport error (arg parse failure)"),
        }
    }

    // ── introspect event_bus output cap (VIC-4) ──────────────────────────────
    #[tokio::test]
    async fn event_bus_caps_output_to_limit() {
        // The full buffers can be tens of thousands of events (megabytes); the action must cap
        // output (default 100, newest first) and still report the true total + a truncated flag.
        use crate::introspection::CapturedTauriEvent;
        let state = state_with(PrivacyConfig::default());
        for i in 0..150 {
            state.event_bus.push(CapturedTauriEvent {
                name: format!("evt-{i}"),
                payload: "{}".to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
            });
        }
        let h = VictauriMcpHandler::new(state, Arc::new(RecordingBridge::default()));

        // Default limit (100).
        let r = call(&h, "introspect", json!({"action": "event_bus"})).await;
        let v: serde_json::Value = serde_json::from_str(&result_text(&r)).unwrap();
        assert_eq!(
            v["tauri_events"]["count"], 150,
            "true total must be reported"
        );
        assert_eq!(v["tauri_events"]["returned"], 100, "default cap is 100");
        assert_eq!(v["tauri_events"]["truncated"], true);
        assert_eq!(v["tauri_events"]["events"].as_array().unwrap().len(), 100);

        // Explicit smaller limit (passed via the generic `args` object).
        let r = call(
            &h,
            "introspect",
            json!({"action": "event_bus", "args": {"limit": 10}}),
        )
        .await;
        let v: serde_json::Value = serde_json::from_str(&result_text(&r)).unwrap();
        assert_eq!(v["tauri_events"]["returned"], 10);
        assert_eq!(v["tauri_events"]["events"].as_array().unwrap().len(), 10);
    }

    // ── recording.replay (audit #30/#31, A1) ─────────────────────────────────

    #[tokio::test]
    async fn replay_never_invokes_a_blocklisted_command() {
        let bridge = RecordingBridge::default();
        let state = state_with(blocking(&["delete_account"]));
        state.recorder.start("s1".to_string()).unwrap();
        state.recorder.record_event(ipc_event("delete_account"));
        let h = VictauriMcpHandler::new(state, Arc::new(bridge.clone()));

        let r = call(&h, "recording", json!({"action": "replay"})).await;

        assert!(
            !bridge.invoked("delete_account"),
            "SIDE-EFFECT LEAK: replay handed a blocklisted command's invoke to the bridge (audit #30/#31)"
        );
        assert!(
            result_text(&r).contains("blocked"),
            "replay should report the command as blocked, got: {}",
            result_text(&r)
        );
    }

    #[tokio::test]
    async fn replay_does_invoke_an_allowed_command() {
        // Positive control: proves the negative test isn't vacuous (the path really
        // reaches the bridge for a permitted command).
        let state = state_with(PrivacyConfig::default());
        let bridge = RecordingBridge::answering(state.pending_evals.clone());
        state.recorder.start("s1".to_string()).unwrap();
        state.recorder.record_event(ipc_event("greet"));
        let h = VictauriMcpHandler::new(state, Arc::new(bridge.clone()));

        let _ = call(&h, "recording", json!({"action": "replay"})).await;

        assert!(
            bridge.invoked("greet"),
            "positive control failed: an ALLOWED command was not invoked, so the negative test proves nothing"
        );
    }

    #[tokio::test]
    async fn imported_session_cannot_invoke_a_blocklisted_command() {
        // audit #31: a crafted session handed to an agent ("replay this to reproduce")
        // must not become arbitrary command invocation.
        let bridge = RecordingBridge::default();
        let state = state_with(blocking(&["wipe_database"]));
        let h = VictauriMcpHandler::new(state, Arc::new(bridge.clone()));

        let session = RecordedSession {
            id: "poisoned".to_string(),
            started_at: chrono::Utc::now(),
            events: vec![RecordedEvent {
                index: 0,
                timestamp: chrono::Utc::now(),
                event: ipc_event("wipe_database"),
            }],
            checkpoints: Vec::new(),
        };
        let session_json = serde_json::to_string(&session).unwrap();

        let imp = call(
            &h,
            "recording",
            json!({"action": "import", "session_json": session_json}),
        )
        .await;
        assert_ne!(
            imp.is_error,
            Some(true),
            "import itself should succeed: {}",
            result_text(&imp)
        );

        let r = call(&h, "recording", json!({"action": "replay"})).await;
        assert!(
            !bridge.invoked("wipe_database"),
            "SIDE-EFFECT LEAK: an imported session replayed a blocklisted command (audit #31)"
        );
        assert!(result_text(&r).contains("blocked"));
    }

    // ── introspect.contract_record / contract_check (audit #30, A2) ───────────

    #[tokio::test]
    async fn contract_record_never_invokes_a_blocklisted_command() {
        let bridge = RecordingBridge::default();
        let state = state_with(blocking(&["delete_account"]));
        let h = VictauriMcpHandler::new(state, Arc::new(bridge.clone()));

        let r = call(
            &h,
            "introspect",
            json!({"action": "contract_record", "command": "delete_account", "args": {"confirm": true}}),
        )
        .await;

        assert!(
            !bridge.invoked("delete_account"),
            "SIDE-EFFECT LEAK: contract_record invoked a blocklisted command (audit #30)"
        );
        assert_eq!(r.is_error, Some(true));
        assert!(
            result_text(&r).contains("blocked by privacy configuration"),
            "got: {}",
            result_text(&r)
        );
    }

    #[tokio::test]
    async fn contract_record_does_invoke_an_allowed_command() {
        let state = state_with(PrivacyConfig::default());
        let bridge = RecordingBridge::answering(state.pending_evals.clone());
        let h = VictauriMcpHandler::new(state, Arc::new(bridge.clone()));

        let _ = call(
            &h,
            "introspect",
            json!({"action": "contract_record", "command": "get_settings"}),
        )
        .await;

        assert!(
            bridge.invoked("get_settings"),
            "positive control failed: contract_record did not invoke an allowed command"
        );
    }

    // ── pending-eval concurrency ceiling (audit: TOCTOU race) ────────────────
    #[tokio::test]
    async fn reserve_pending_is_a_hard_ceiling_under_concurrency() {
        // A check-then-insert (lock, read len(), unlock, …, lock, insert) races: many
        // concurrent callers all pass a STALE len() check before any inserts, blowing past
        // MAX_PENDING_EVALS. `reserve_pending` checks AND inserts under one lock, so the cap
        // is a true ceiling. Pre-fill to MAX-5, fire 50 concurrent reservations: EXACTLY 5
        // may succeed and the map must NEVER exceed the cap.
        let state = state_with(PrivacyConfig::default());
        {
            let mut p = state.pending_evals.lock().await;
            for i in 0..(MAX_PENDING_EVALS - 5) {
                let (tx, _rx) = tokio::sync::oneshot::channel();
                p.insert(format!("pre-{i}"), tx);
            }
        }
        let h = Arc::new(VictauriMcpHandler::new(
            state.clone(),
            Arc::new(RecordingBridge::default()),
        ));
        let mut tasks = Vec::new();
        for i in 0..50 {
            let h = h.clone();
            tasks.push(tokio::spawn(async move {
                let (tx, _rx) = tokio::sync::oneshot::channel();
                // keep rx alive until the reservation has been decided
                let ok = h.reserve_pending(&format!("c-{i}"), tx).await.is_ok();
                (ok, _rx)
            }));
        }
        let mut granted = 0;
        let mut keep = Vec::new();
        for t in tasks {
            let (ok, rx) = t.await.unwrap();
            if ok {
                granted += 1;
            }
            keep.push(rx); // hold receivers so reserved entries are not dropped/removed
        }
        let len = state.pending_evals.lock().await.len();
        assert!(
            len <= MAX_PENDING_EVALS,
            "ceiling breached: {len} > {MAX_PENDING_EVALS}"
        );
        assert_eq!(
            granted, 5,
            "exactly the 5 free slots should have been reserved, got {granted}"
        );
        drop(keep);
    }

    #[tokio::test]
    async fn contract_check_never_reinvokes_a_now_blocklisted_command() {
        // A baseline recorded before the command was blocked must not be re-invoked
        // once the operator adds it to the blocklist (audit #30).
        let bridge = RecordingBridge::default();
        let state = state_with(blocking(&["delete_account"]));
        state
            .contract_store
            .record(crate::introspection::ContractBaseline {
                command: "delete_account".to_string(),
                args: json!({}),
                shape: crate::introspection::JsonShape::from_value(&json!(true)),
                sample: "true".to_string(),
                recorded_at: chrono_now(),
            });
        let h = VictauriMcpHandler::new(state, Arc::new(bridge.clone()));

        let _ = call(&h, "introspect", json!({"action": "contract_check"})).await;

        assert!(
            !bridge.invoked("delete_account"),
            "SIDE-EFFECT LEAK: contract_check re-invoked a now-blocklisted command (audit #30)"
        );
    }

    // ── MCP resources honour the privacy gate (audit B1) ──────────────────────

    #[test]
    fn resource_reads_are_gated_by_their_mirrored_capability() {
        // Resources bypass the tool dispatcher, so the read path must apply the same
        // gate. Disabling the capability a resource mirrors must block the resource.
        let cfg = PrivacyConfig {
            disabled_tools: HashSet::from([
                "logs.ipc".to_string(),
                "window.list".to_string(),
                "get_plugin_info".to_string(),
            ]),
            ..Default::default()
        };
        for uri in [
            RESOURCE_URI_IPC_LOG,
            RESOURCE_URI_WINDOWS,
            RESOURCE_URI_STATE,
        ] {
            let cap = resource_required_capability(uri).expect("resource maps to a capability");
            assert!(
                !cfg.is_tool_enabled(cap),
                "disabling capability {cap} must gate resource {uri} (audit B1)"
            );
        }
        // Sanity: with nothing disabled, all three resources read.
        let full = PrivacyConfig::default();
        for uri in [
            RESOURCE_URI_IPC_LOG,
            RESOURCE_URI_WINDOWS,
            RESOURCE_URI_STATE,
        ] {
            assert!(full.is_tool_enabled(resource_required_capability(uri).unwrap()));
        }
    }

    // ── empty/whitespace auth token collapses to NO auth (audit B2) ───────────

    #[tokio::test]
    async fn empty_auth_token_collapses_to_no_auth() {
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        for token in [Some(String::new()), Some("   ".to_string())] {
            let app = crate::mcp::server::build_app_full(
                state_with(PrivacyConfig::default()),
                Arc::new(RecordingBridge::default()),
                token.clone(),
                None,
            );
            let req = axum::extract::Request::builder()
                .uri("/info")
                .header("host", "127.0.0.1")
                .body(axum::body::Body::empty())
                .unwrap();
            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                200,
                "/info must be reachable with empty token {token:?} (no auth layer)"
            );
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                body["auth_required"],
                json!(false),
                "empty/whitespace token must report auth_required:false, not looks-protected-isnt (audit B2); token={token:?}"
            );
        }
    }

    // ── app_info env allowlist drops secrets (audit #5/B3) ────────────────────

    #[test]
    fn is_safe_env_key_drops_secrets_keeps_safe() {
        for secret in [
            "VICTAURI_AUTH_TOKEN",
            "TAURI_SIGNING_PRIVATE_KEY",
            "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
            "CARGO_REGISTRY_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "DATABASE_DSN",
            "GH_PAT",
        ] {
            assert!(
                !is_safe_env_key(secret),
                "{secret} is secret-shaped and must NOT be surfaced by app_info (audit #5)"
            );
        }
        for safe in [
            "HOME",
            "LANG",
            "TERM",
            "XDG_RUNTIME_DIR",
            "TAURI_ENV_PLATFORM",
        ] {
            assert!(
                is_safe_env_key(safe),
                "{safe} should be surfaced by app_info"
            );
        }
    }
}

/// `screenshot` must refuse to "capture" a non-visible window.
///
/// Live-4DA dogfood (2026-06-16): requesting a hidden window (`label:"briefing"`)
/// returned a PNG that was actually the MAIN window's pixels — the OS capture path has
/// no live surface for an unmapped window, so it silently yields stale/foreign content.
/// The tool now checks visibility first and fails with an actionable message instead.
#[cfg(test)]
mod screenshot_visibility_tests {
    use super::*;
    use crate::bridge::WebviewBridge;
    use crate::privacy::PrivacyConfig;
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;
    use victauri_core::{CommandRegistry, EventLog, EventRecorder, WindowState};

    fn window(label: &str, visible: bool) -> WindowState {
        WindowState {
            label: label.to_string(),
            title: label.to_string(),
            url: "http://localhost/".to_string(),
            visible,
            focused: false,
            maximized: false,
            minimized: false,
            fullscreen: false,
            position: (0, 0),
            size: (800, 600),
        }
    }

    /// A bridge with a configurable window set that RECORDS the label `get_native_handle`
    /// is asked for, then errs. Recording the label lets a test assert WHICH window the
    /// screenshot tool resolved to (the audit-P2 case: omitted label must resolve to a
    /// VISIBLE window, never hidden "main"); the error lets a test prove the visibility gate
    /// fired *before* the OS-handle path was reached.
    struct ConfigBridge {
        windows: Vec<WindowState>,
        handle_label: Arc<StdMutex<Option<Option<String>>>>,
    }

    impl ConfigBridge {
        fn new(windows: Vec<WindowState>) -> Self {
            Self {
                windows,
                handle_label: Arc::new(StdMutex::new(None)),
            }
        }
        /// The label `get_native_handle` was called with, if it was reached.
        /// `Some(Some(l))` = called with label `l`; `Some(None)` = called with the default;
        /// `None` = never reached (gate short-circuited).
        fn requested_handle(&self) -> Option<Option<String>> {
            self.handle_label
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    impl WebviewBridge for ConfigBridge {
        fn eval_webview(&self, _l: Option<&str>, _s: &str) -> Result<(), String> {
            Err("no eval".to_string())
        }
        fn get_window_states(&self, label: Option<&str>) -> Vec<WindowState> {
            match label {
                Some(l) => self
                    .windows
                    .iter()
                    .filter(|w| w.label == l)
                    .cloned()
                    .collect(),
                None => self.windows.clone(),
            }
        }
        fn list_window_labels(&self) -> Vec<String> {
            self.windows.iter().map(|w| w.label.clone()).collect()
        }
        fn get_native_handle(&self, l: Option<&str>) -> Result<isize, String> {
            *self
                .handle_label
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(l.map(str::to_string));
            Err("native handle path reached".to_string())
        }
        fn manage_window(&self, _l: Option<&str>, _a: &str) -> Result<String, String> {
            Ok(String::new())
        }
        fn resize_window(&self, _l: Option<&str>, _w: u32, _h: u32) -> Result<(), String> {
            Ok(())
        }
        fn move_window(&self, _l: Option<&str>, _x: i32, _y: i32) -> Result<(), String> {
            Ok(())
        }
        fn set_window_title(&self, _l: Option<&str>, _t: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn handler_with(bridge: Arc<ConfigBridge>) -> VictauriMcpHandler {
        let state = Arc::new(VictauriState {
            event_log: EventLog::new(100),
            registry: CommandRegistry::new(),
            port: std::sync::atomic::AtomicU16::new(0),
            pending_evals: Arc::new(Mutex::new(HashMap::new())),
            recorder: EventRecorder::new(100),
            privacy: PrivacyConfig::default(),
            eval_timeout: std::time::Duration::from_millis(100),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            started_at: std::time::Instant::now(),
            tool_invocations: std::sync::atomic::AtomicU64::new(0),
            allow_file_navigation: false,
            command_timings: crate::introspection::CommandTimings::new(),
            fault_registry: crate::introspection::FaultRegistry::new(),
            contract_store: crate::introspection::ContractStore::new(),
            startup_timeline: crate::introspection::StartupTimeline::new(),
            event_bus: crate::introspection::EventBusMonitor::default(),
            task_tracker: crate::introspection::TaskTracker::new(),
            bridge_ready: std::sync::atomic::AtomicBool::new(true),
            bridge_notify: tokio::sync::Notify::new(),
            db_search_paths: Vec::new(),
            screencast: Arc::new(crate::screencast::Screencast::default()),
            probes: crate::introspection::AppStateProbes::default(),
        });
        VictauriMcpHandler::new(state, bridge)
    }

    fn error_text(r: &CallToolResult) -> String {
        r.content
            .iter()
            .filter_map(|c| match &c.raw {
                RawContent::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn hidden_window_screenshot_errors_clearly() {
        let bridge = Arc::new(ConfigBridge::new(vec![
            window("main", true),
            window("briefing", false),
        ]));
        let h = handler_with(bridge.clone());
        let r = h
            .screenshot(Parameters(ScreenshotParams {
                window_label: Some("briefing".to_string()),
            }))
            .await;
        assert_eq!(r.is_error, Some(true), "hidden window must error");
        let text = error_text(&r);
        assert!(
            text.contains("not visible"),
            "error must explain the window is not visible, got: {text}"
        );
        assert!(
            bridge.requested_handle().is_none(),
            "must short-circuit BEFORE the OS-handle/capture path"
        );
    }

    #[tokio::test]
    async fn visible_window_screenshot_proceeds_to_capture() {
        let bridge = Arc::new(ConfigBridge::new(vec![
            window("main", true),
            window("briefing", false),
        ]));
        let h = handler_with(bridge.clone());
        let r = h
            .screenshot(Parameters(ScreenshotParams {
                window_label: Some("main".to_string()),
            }))
            .await;
        // The gate must let a visible window THROUGH to the OS-handle path (which this mock
        // fails) — proving the gate only blocks hidden windows.
        let text = error_text(&r);
        assert!(
            text.contains("native handle path reached")
                || text.contains("cannot get window handle"),
            "a visible window must reach the capture path, got: {text}"
        );
        assert_eq!(
            bridge.requested_handle(),
            Some(Some("main".to_string())),
            "must capture the explicitly requested visible window"
        );
    }

    // GPT audit (P2): `screenshot {}` with NO label previously resolved through
    // find_window(None), which prefers "main" UNCONDITIONALLY — so an app that hides main but
    // keeps a secondary window visible captured hidden main (the wrong-pixels class the PR
    // exists to prevent). The tool must now resolve its own VISIBLE target.
    #[tokio::test]
    async fn omitted_label_skips_hidden_main_for_visible_secondary() {
        let bridge = Arc::new(ConfigBridge::new(vec![
            window("main", false),     // main hidden
            window("secondary", true), // a different window is visible
        ]));
        let h = handler_with(bridge.clone());
        let r = h
            .screenshot(Parameters(ScreenshotParams { window_label: None }))
            .await;
        let text = error_text(&r);
        assert!(
            !text.contains("not visible") && !text.contains("no visible window"),
            "a visible secondary window exists — must NOT error, got: {text}"
        );
        assert_eq!(
            bridge.requested_handle(),
            Some(Some("secondary".to_string())),
            "omitted label must resolve to the VISIBLE secondary, never hidden main"
        );
    }

    // Omitted label with a visible main present must still prefer "main".
    #[tokio::test]
    async fn omitted_label_prefers_visible_main() {
        let bridge = Arc::new(ConfigBridge::new(vec![
            window("main", true),
            window("secondary", true),
        ]));
        let h = handler_with(bridge.clone());
        let _ = h
            .screenshot(Parameters(ScreenshotParams { window_label: None }))
            .await;
        assert_eq!(
            bridge.requested_handle(),
            Some(Some("main".to_string())),
            "with a visible main present, omitted label must resolve to main"
        );
    }

    // Every window hidden + omitted label: error clearly, never capture a hidden window.
    #[tokio::test]
    async fn all_hidden_omitted_label_errors() {
        let bridge = Arc::new(ConfigBridge::new(vec![
            window("main", false),
            window("briefing", false),
        ]));
        let h = handler_with(bridge.clone());
        let r = h
            .screenshot(Parameters(ScreenshotParams { window_label: None }))
            .await;
        assert_eq!(r.is_error, Some(true), "all-hidden must error");
        assert!(
            error_text(&r).contains("no visible window"),
            "error must say there is no visible window, got: {}",
            error_text(&r)
        );
        assert!(
            bridge.requested_handle().is_none(),
            "must NOT reach the OS-handle path when every window is hidden"
        );
    }

    // An unknown explicit label is passed THROUGH to get_native_handle (which produces the
    // canonical "window not found"), not rejected as "not visible".
    #[tokio::test]
    async fn unknown_label_falls_through_to_handle_resolution() {
        let bridge = Arc::new(ConfigBridge::new(vec![window("main", true)]));
        let h = handler_with(bridge.clone());
        let r = h
            .screenshot(Parameters(ScreenshotParams {
                window_label: Some("ghost".to_string()),
            }))
            .await;
        assert!(
            !error_text(&r).contains("not visible"),
            "unknown label must not be reported as 'not visible'"
        );
        assert_eq!(
            bridge.requested_handle(),
            Some(Some("ghost".to_string())),
            "unknown label must be forwarded verbatim to get_native_handle"
        );
    }
}
