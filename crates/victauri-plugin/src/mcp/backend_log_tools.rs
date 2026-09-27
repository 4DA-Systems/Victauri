//! Tool bodies for the backend-log surface: `logs backend` / `backend_digest` /
//! `stdout`, `wait_for {condition:"log"}`, and `invoke_command {with_logs}`.
//!
//! Kept out of `mod.rs` (the rmcp `#[tool_router]` block) so the dispatch there
//! is one-line arms; everything here is a plain inherent method.

use rmcp::model::CallToolResult;

use super::VictauriMcpHandler;
use super::compound_params::LogsParams;
use super::helpers::{json_result, missing_param, tool_error};
use super::other_params::WaitForParams;
use crate::backend_logs::{self, LogLevel, LogQuery};

/// Default page size for `logs backend` / `logs stdout`.
const DEFAULT_BACKEND_LIMIT: usize = 100;
/// Hard cap on a page, whatever the caller asks for.
const MAX_BACKEND_LIMIT: usize = 2_000;
/// How much of the console capture a cursor-less `logs stdout` reads.
const STDOUT_TAIL_BYTES: u64 = 256 * 1024;
/// Most entries attached to an `invoke_command {with_logs:true}` result.
const MAX_ATTACHED_LOGS: usize = 100;

/// Guidance returned whenever an agent asks for backend logs that no source feeds.
pub(super) const ENABLE_HINT: &str = "No backend log source is active in this app. Pick one: \
(1) zero code — launch the app with `victauri run -- <your dev command>` (e.g. \
`victauri run -- npm run tauri dev`); raw stdout/stderr then appear under `logs stdout`, \
including crash output; (2) one line for structured logs — add `.with(victauri_plugin::log_layer())` \
to the app's tracing_subscriber registry, or chain `victauri_plugin::log_logger()` into its `log` \
setup (tauri-plugin-log / fern) / wrap its logger with `victauri_plugin::wrap_logger(...)`. \
Panics are captured automatically.";

fn parse_level(raw: Option<&str>) -> Result<Option<LogLevel>, CallToolResult> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => LogLevel::parse(s).map(Some).ok_or_else(|| {
            tool_error(format!(
                "unknown level '{s}' — use trace, debug, info, warn or error"
            ))
        }),
    }
}

fn field_pairs(
    fields: Option<&std::collections::BTreeMap<String, serde_json::Value>>,
) -> Vec<(String, serde_json::Value)> {
    fields
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

fn clamp_limit(limit: Option<usize>) -> usize {
    limit
        .unwrap_or(DEFAULT_BACKEND_LIMIT)
        .clamp(1, MAX_BACKEND_LIMIT)
}

fn sources_json() -> serde_json::Value {
    let s = backend_logs::active_sources();
    serde_json::json!({
        "tracing": s.tracing,
        "log": s.log,
        "panic_hook": s.panic_hook,
        "stdout_capture": s.stdout_capture,
        // Where `victauri run` writes the raw output — lets a follower keep reading it
        // after the app dies (the crash's last words and exit status land there).
        "stdout_capture_path": backend_logs::console_capture_path().map(|p| p.to_string_lossy().into_owned()),
    })
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn since_ms_from(since: Option<f64>) -> Option<u64> {
    since
        .filter(|v| v.is_finite() && *v > 0.0)
        .map(|v| v as u64)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl VictauriMcpHandler {
    pub(super) fn logs_backend(&self, params: &LogsParams) -> CallToolResult {
        let min_level = match parse_level(params.level.as_deref()) {
            Ok(l) => l.unwrap_or(LogLevel::Trace),
            Err(e) => return e,
        };
        let query = LogQuery {
            min_level,
            targets: params
                .target
                .as_deref()
                .map(|t| {
                    t.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            contains: params.filter.clone().filter(|s| !s.is_empty()),
            since_seq: params.since_seq,
            since_ms: since_ms_from(params.since),
            fields: field_pairs(params.fields.as_ref()),
            limit: clamp_limit(params.limit),
        };
        let page = self.state.backend_logs.query(&query);
        let sources = backend_logs::active_sources();
        let mut out = serde_json::to_value(&page).unwrap_or_default();
        if let Some(obj) = out.as_object_mut() {
            obj.insert("sources".into(), sources_json());
            if page.entries.is_empty() && page.next_seq == 0 {
                let hint = if sources.any_structured() {
                    "Backend capture is active but nothing has been logged yet.".to_string()
                } else if sources.stdout_capture {
                    "No structured backend source is installed, but the app runs under \
                     `victauri run` — read its raw output with `logs {action:\"stdout\"}`."
                        .to_string()
                } else {
                    ENABLE_HINT.to_string()
                };
                obj.insert("hint".into(), hint.into());
            }
        }
        json_result(&out)
    }

    pub(super) fn logs_backend_digest(&self, params: &LogsParams) -> CallToolResult {
        let top = params.limit.unwrap_or(10).clamp(1, 100);
        let digest = self.state.backend_logs.digest(top, top);
        let mut out = serde_json::to_value(&digest).unwrap_or_default();
        if let Some(obj) = out.as_object_mut() {
            obj.insert("sources".into(), sources_json());
            let sources = backend_logs::active_sources();
            if !sources.any_structured() {
                obj.insert(
                    "hint".into(),
                    if sources.stdout_capture {
                        "Only panics are captured structurally; the app's raw output is under \
                         `logs {action:\"stdout\"}`."
                    } else {
                        ENABLE_HINT
                    }
                    .into(),
                );
            }
        }
        json_result(&out)
    }

    pub(super) fn logs_stdout(params: &LogsParams) -> CallToolResult {
        let Some(path) = backend_logs::console_capture_path() else {
            return tool_error(
                "This app was not launched with `victauri run`, so its raw stdout/stderr is \
                 not captured. Restart it as `victauri run -- <your dev command>` (zero app \
                 changes), or read structured logs via `logs {action:\"backend\"}`.",
            );
        };
        let min_level = match parse_level(params.level.as_deref()) {
            Ok(l) => l,
            Err(e) => return e,
        };
        let limit = clamp_limit(params.limit);
        let needle = params
            .filter
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase);
        match backend_logs::read_console_capture(&path, params.since_seq, STDOUT_TAIL_BYTES) {
            Ok((lines, next)) => {
                let mut matched: Vec<_> = lines
                    .into_iter()
                    .filter(|l| {
                        min_level.is_none_or(|min| l.level.is_some_and(|lv| lv >= min))
                            && needle
                                .as_deref()
                                .is_none_or(|n| l.text.to_lowercase().contains(n))
                    })
                    .collect();
                let total = matched.len();
                if total > limit {
                    matched.drain(..total - limit);
                }
                json_result(&serde_json::json!({
                    "lines": matched,
                    "total_matched": total,
                    "truncated": total > limit,
                    "next_seq": next,
                    "path": path.to_string_lossy(),
                }))
            }
            Err(e) => tool_error(format!(
                "cannot read the console capture at {}: {e}",
                path.display()
            )),
        }
    }

    /// `wait_for {condition:"log"}` — edge-triggered on the capture buffer; falls
    /// back to polling the `victauri run` console capture when no structured
    /// source is installed.
    pub(super) async fn wait_for_log(
        &self,
        params: &WaitForParams,
        timeout_ms: u64,
        poll_ms: u64,
    ) -> CallToolResult {
        let Some(needle) = params
            .value
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase)
        else {
            return missing_param("value", "wait_for(log)");
        };
        let min_level = match parse_level(params.level.as_deref()) {
            Ok(l) => l.unwrap_or(LogLevel::Trace),
            Err(e) => return e,
        };
        let since_ms = params.since_ms.unwrap_or(2000);
        let wanted_fields = field_pairs(params.fields.as_ref());
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(timeout_ms);
        let sources = backend_logs::active_sources();

        if !sources.any_structured() && sources.stdout_capture {
            return Self::wait_for_stdout_line(&needle, min_level, since_ms, timeout, poll_ms)
                .await;
        }

        // since_ms == 0 means "only lines logged after this call" — use the cursor,
        // not a timestamp, so a line from this same millisecond cannot count.
        // An explicit cursor wins: it is exact, where a time look-back can also catch
        // a previous run's identical line.
        let from = if let Some(seq) = params.since_seq {
            seq
        } else if since_ms == 0 {
            self.state.backend_logs.next_seq()
        } else {
            self.state
                .backend_logs
                .seq_at_or_after(now_ms().saturating_sub(since_ms))
        };
        let hit = self
            .state
            .backend_logs
            .wait_for(from, timeout, |e| {
                e.level >= min_level
                    && backend_logs::entry_contains(e, &needle)
                    && backend_logs::entry_fields_match(e, &wanted_fields)
            })
            .await;
        let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        if let Some(entry) = hit {
            return json_result(&serde_json::json!({
                "ok": true,
                "entry": entry,
                "elapsed_ms": elapsed_ms,
            }));
        }
        let mut out = serde_json::json!({
            "ok": false,
            "error": format!("timeout after {timeout_ms}ms"),
            "elapsed_ms": elapsed_ms,
            "sources": sources_json(),
        });
        if !sources.any_structured() {
            out["hint"] = ENABLE_HINT.into();
        }
        json_result(&out)
    }

    async fn wait_for_stdout_line(
        needle: &str,
        min_level: LogLevel,
        since_ms: u64,
        timeout: std::time::Duration,
        poll_ms: u64,
    ) -> CallToolResult {
        let Some(path) = backend_logs::console_capture_path() else {
            return tool_error(ENABLE_HINT);
        };
        let start = std::time::Instant::now();
        let cutoff = now_ms().saturating_sub(since_ms);
        // Start from a bounded tail so the look-back works without rescanning a huge file.
        let mut cursor: Option<u64> = None;
        loop {
            match backend_logs::read_console_capture(&path, cursor, STDOUT_TAIL_BYTES) {
                Ok((lines, next)) => {
                    if let Some(line) = lines.into_iter().find(|l| {
                        l.ts_ms >= cutoff
                            && (min_level == LogLevel::Trace
                                || l.level.is_some_and(|lv| lv >= min_level))
                            && l.text.to_lowercase().contains(needle)
                    }) {
                        return json_result(&serde_json::json!({
                            "ok": true,
                            "line": line,
                            "source": "stdout",
                            "elapsed_ms": u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                        }));
                    }
                    cursor = Some(next);
                }
                Err(e) => {
                    return tool_error(format!(
                        "cannot read the console capture at {}: {e}",
                        path.display()
                    ));
                }
            }
            if start.elapsed() >= timeout {
                return json_result(&serde_json::json!({
                    "ok": false,
                    "error": format!("timeout after {}ms", timeout.as_millis()),
                    "source": "stdout",
                }));
            }
            tokio::time::sleep(std::time::Duration::from_millis(poll_ms.clamp(20, 1000))).await;
        }
    }

    /// Cursor to take before running a command whose logs should be attached.
    pub(super) fn backend_log_cursor(&self) -> u64 {
        self.state.backend_logs.next_seq()
    }

    /// Wrap a successful `invoke_command` result with the backend entries
    /// captured since `from`.
    pub(super) fn attach_backend_logs(&self, raw_result: &str, from: u64) -> CallToolResult {
        let to = self.state.backend_logs.next_seq();
        let (entries, total) =
            self.state
                .backend_logs
                .range(from, to, LogLevel::Debug, MAX_ATTACHED_LOGS);
        let result: serde_json::Value = serde_json::from_str(raw_result)
            .unwrap_or_else(|_| serde_json::Value::String(raw_result.to_string()));
        let mut out = serde_json::json!({
            "result": result,
            "backend_logs": entries,
            // Sequence number at the moment the command started: pass it to
            // `wait_for {condition:"log", since_seq}` to await THIS call's later lines
            // (fire-and-forget work) without ever matching an earlier run's.
            "backend_log_cursor": from,
            "next_seq": to,
        });
        if total > entries.len() {
            out["backend_logs_truncated"] = (total - entries.len()).into();
        }
        if !backend_logs::active_sources().any_structured() {
            out["backend_logs_hint"] = ENABLE_HINT.into();
        }
        json_result(&out)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rmcp::model::CallToolResult;
    use serde_json::json;
    use victauri_core::WindowState;

    use super::super::VictauriMcpHandler;
    use crate::VictauriState;
    use crate::backend_logs::{BackendLogLayer, LogBuffer};
    use crate::bridge::WebviewBridge;

    /// A bridge with no webview: the backend-log tools must never need one.
    struct NoWebview;

    impl WebviewBridge for NoWebview {
        fn eval_webview(&self, _: Option<&str>, _: &str) -> Result<(), String> {
            Err("no webview in this test".into())
        }
        fn get_window_states(&self, _: Option<&str>) -> Vec<WindowState> {
            Vec::new()
        }
        fn list_window_labels(&self) -> Vec<String> {
            Vec::new()
        }
        fn get_native_handle(&self, _: Option<&str>) -> Result<isize, String> {
            Err("none".into())
        }
        fn manage_window(&self, _: Option<&str>, _: &str) -> Result<String, String> {
            Err("none".into())
        }
        fn resize_window(&self, _: Option<&str>, _: u32, _: u32) -> Result<(), String> {
            Err("none".into())
        }
        fn move_window(&self, _: Option<&str>, _: i32, _: i32) -> Result<(), String> {
            Err("none".into())
        }
        fn set_window_title(&self, _: Option<&str>, _: &str) -> Result<(), String> {
            Err("none".into())
        }
    }

    fn handler() -> (VictauriMcpHandler, Arc<LogBuffer>) {
        let state = VictauriState::for_tests();
        let buf = Arc::clone(&state.backend_logs);
        (
            VictauriMcpHandler::new(Arc::new(state), Arc::new(NoWebview)),
            buf,
        )
    }

    fn text(r: &CallToolResult) -> String {
        r.content
            .iter()
            .filter_map(|c| match c {
                rmcp::model::ContentBlock::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn call(
        h: &VictauriMcpHandler,
        tool: &str,
        args: serde_json::Value,
    ) -> serde_json::Value {
        let r = h
            .execute_tool(tool, args)
            .await
            .unwrap_or_else(|_| panic!("dispatch failed for {tool}"));
        assert_ne!(r.is_error, Some(true), "{tool} errored: {}", text(&r));
        serde_json::from_str(&text(&r)).unwrap()
    }

    fn emit(buf: &Arc<LogBuffer>, f: impl FnOnce()) {
        use tracing_subscriber::layer::SubscriberExt;
        let sub =
            tracing_subscriber::registry().with(BackendLogLayer::with_buffer(Arc::clone(buf)));
        tracing::subscriber::with_default(sub, f);
    }

    #[tokio::test]
    async fn logs_backend_pages_with_filters_and_cursor() {
        let (h, buf) = handler();
        emit(&buf, || {
            tracing::info!(target: "app::sync", items = 3_u64, "sync started");
            tracing::warn!(target: "app::sync", "sync slow");
            tracing::error!(target: "app::db", code = 5_i64, "write failed");
        });

        let all = call(&h, "logs", json!({"action": "backend"})).await;
        assert_eq!(all["entries"].as_array().unwrap().len(), 3);
        assert_eq!(all["next_seq"], 3);
        assert_eq!(all["entries"][0]["fields"]["items"], 3);

        let warn = call(&h, "logs", json!({"action": "backend", "level": "warn"})).await;
        assert_eq!(warn["entries"].as_array().unwrap().len(), 2);

        let db = call(
            &h,
            "logs",
            json!({"action": "backend", "target": "app::db, nope"}),
        )
        .await;
        assert_eq!(db["entries"].as_array().unwrap().len(), 1);
        assert_eq!(db["entries"][0]["message"], "write failed");

        let newer = call(&h, "logs", json!({"action": "backend", "since_seq": 2})).await;
        assert_eq!(newer["entries"].as_array().unwrap().len(), 1);

        let found = call(&h, "logs", json!({"action": "backend", "filter": "SLOW"})).await;
        assert_eq!(found["entries"][0]["message"], "sync slow");
    }

    #[tokio::test]
    async fn logs_backend_rejects_an_unknown_level_clearly() {
        let (h, _) = handler();
        let r = h
            .execute_tool("logs", json!({"action": "backend", "level": "loud"}))
            .await
            .unwrap_or_else(|_| panic!("dispatch failed"));
        assert_eq!(r.is_error, Some(true));
        assert!(text(&r).contains("unknown level"));
    }

    #[tokio::test]
    async fn logs_backend_digest_summarises() {
        let (h, buf) = handler();
        emit(&buf, || {
            for i in 0..5 {
                tracing::info!(target: "app::gate", "policy changed cpu={i}");
            }
            tracing::warn!(target: "app::scoring", "slow build");
        });
        let d = call(&h, "logs", json!({"action": "backend_digest"})).await;
        assert_eq!(d["buffered"], 6);
        assert_eq!(d["repeated"][0]["count"], 5);
        assert_eq!(d["recent_problems"].as_array().unwrap().len(), 1);
        assert!(d["sources"].is_object());
    }

    #[tokio::test]
    async fn wait_for_log_wakes_on_a_later_entry() {
        let (h, buf) = handler();
        let producer = Arc::clone(&buf);
        let bg = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            emit(&producer, || {
                tracing::info!(target: "app::pipeline", run = 7_u64, "pipeline complete");
            });
        });
        let started = std::time::Instant::now();
        let r = call(
            &h,
            "wait_for",
            json!({"condition": "log", "value": "Pipeline Complete", "timeout_ms": 5000}),
        )
        .await;
        bg.await.unwrap();
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["entry"]["fields"]["run"], 7);
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
    }

    #[tokio::test]
    async fn wait_for_log_honours_min_level_and_times_out_with_context() {
        let (h, buf) = handler();
        emit(&buf, || tracing::info!(target: "app", "disk almost full"));
        let r = call(
            &h,
            "wait_for",
            json!({"condition": "log", "value": "disk", "level": "error", "timeout_ms": 150}),
        )
        .await;
        assert_eq!(r["ok"], false, "an info line must not satisfy level=error");
        assert!(r["error"].as_str().unwrap().contains("timeout"));
    }

    #[tokio::test]
    async fn wait_for_log_look_back_catches_a_line_logged_just_before() {
        let (h, buf) = handler();
        emit(&buf, || tracing::info!(target: "app", "migration done"));
        let r = call(
            &h,
            "wait_for",
            json!({"condition": "log", "value": "migration done", "timeout_ms": 200}),
        )
        .await;
        assert_eq!(r["ok"], true, "default 2s look-back must see it: {r}");
        let none = call(
            &h,
            "wait_for",
            json!({"condition": "log", "value": "migration done", "timeout_ms": 150, "since_ms": 0}),
        )
        .await;
        assert_eq!(none["ok"], false, "since_ms=0 means only new lines count");
    }

    /// Found live on 4DA: a foreground analysis and the app's own background analysis
    /// ran concurrently and both logged "Cache-first analysis finished"; a text-only
    /// wait matched the WRONG run. Field matching must pick the caller's run.
    #[tokio::test]
    async fn wait_for_log_fields_select_the_right_concurrent_run() {
        let (h, buf) = handler();
        emit(&buf, || {
            tracing::info!(target: "app::analysis", run_type = "background_deep", elapsed_ms = 96_789_u64, "Cache-first analysis finished");
        });
        let producer = Arc::clone(&buf);
        let bg = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            emit(&producer, || {
                tracing::info!(target: "app::analysis", run_type = "foreground_fast", elapsed_ms = 68_787_u64, "Cache-first analysis finished");
            });
        });
        let r = call(
            &h,
            "wait_for",
            json!({
                "condition": "log",
                "value": "analysis finished",
                "fields": {"run_type": "FOREGROUND_FAST"},
                "timeout_ms": 5000,
            }),
        )
        .await;
        bg.await.unwrap();
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["entry"]["fields"]["run_type"], "foreground_fast");

        // The same selector works when paging, and numbers compare as text.
        let page = call(
            &h,
            "logs",
            json!({"action": "backend", "fields": {"elapsed_ms": 96_789}}),
        )
        .await;
        assert_eq!(page["entries"].as_array().unwrap().len(), 1);
        assert_eq!(page["entries"][0]["fields"]["run_type"], "background_deep");
    }

    /// Repeated runs of one code path log identical lines: a cursor taken before the
    /// run must exclude the previous run's line even inside the time look-back.
    #[tokio::test]
    async fn wait_for_log_since_seq_ignores_a_previous_runs_identical_line() {
        let (h, buf) = handler();
        emit(
            &buf,
            || tracing::info!(target: "app", run = 1_u64, "pipeline complete"),
        );
        let cursor = buf.next_seq();
        let producer = Arc::clone(&buf);
        let bg = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            emit(
                &producer,
                || tracing::info!(target: "app", run = 2_u64, "pipeline complete"),
            );
        });
        let r = call(
            &h,
            "wait_for",
            json!({"condition": "log", "value": "pipeline complete", "since_seq": cursor, "timeout_ms": 5000}),
        )
        .await;
        bg.await.unwrap();
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(
            r["entry"]["fields"]["run"], 2,
            "must be THIS run's line: {r}"
        );
    }

    #[tokio::test]
    async fn wait_for_log_requires_a_value() {
        let (h, _) = handler();
        let r = h
            .execute_tool("wait_for", json!({"condition": "log"}))
            .await
            .unwrap_or_else(|_| panic!("dispatch failed"));
        assert_eq!(r.is_error, Some(true));
    }
}
