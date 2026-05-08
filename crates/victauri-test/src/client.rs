use serde_json::{Value, json};

use crate::assertions::VerifyBuilder;
use crate::error::TestError;
use crate::visual::{VisualDiff, VisualOptions};

/// Typed HTTP client for the Victauri MCP server.
///
/// Manages session lifecycle (initialize → tool calls → cleanup) and provides
/// convenient methods for common test operations.
pub struct VictauriClient {
    http: reqwest::Client,
    base_url: String,
    session_id: String,
    next_id: u64,
    auth_token: Option<String>,
}

impl VictauriClient {
    /// Connect to a Victauri MCP server on the given port.
    /// Sends `initialize` and `notifications/initialized` automatically.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Connection`] if the server is unreachable or
    /// returns a non-success status. Returns [`TestError::Request`] on
    /// HTTP transport failures.
    pub async fn connect(port: u16) -> Result<Self, TestError> {
        Self::connect_with_token(port, None).await
    }

    /// Connect with an optional Bearer auth token.
    ///
    /// Retries up to 3 times with exponential backoff on 429 (rate limited).
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Connection`] if the server is unreachable or
    /// returns a non-success status. Returns [`TestError::Request`] on
    /// HTTP transport failures.
    pub async fn connect_with_token(port: u16, token: Option<&str>) -> Result<Self, TestError> {
        let base_url = format!("http://127.0.0.1:{port}");
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| TestError::Connection(e.to_string()))?;

        let init_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "victauri-test", "version": env!("CARGO_PKG_VERSION")}
            }
        });

        let mut init_resp = None;
        for attempt in 0..4 {
            let mut req = http
                .post(format!("{base_url}/mcp"))
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .json(&init_body);
            if let Some(t) = token {
                req = req.header("Authorization", format!("Bearer {t}"));
            }

            let resp = req
                .send()
                .await
                .map_err(|e| TestError::Connection(e.to_string()))?;

            if resp.status() == 429 && attempt < 3 {
                let delay = std::time::Duration::from_millis(100 * (1 << attempt));
                tokio::time::sleep(delay).await;
                continue;
            }

            init_resp = Some(resp);
            break;
        }

        let init_resp = init_resp
            .ok_or_else(|| TestError::Connection("initialize failed after retries".into()))?;

        if !init_resp.status().is_success() {
            return Err(TestError::Connection(format!(
                "initialize returned {}",
                init_resp.status()
            )));
        }

        let session_id = init_resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| TestError::Connection("no mcp-session-id header".into()))?
            .to_string();

        let mut notify_req = http
            .post(format!("{base_url}/mcp"))
            .header("Content-Type", "application/json")
            .header("mcp-session-id", &session_id)
            .json(&json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }));

        if let Some(t) = token {
            notify_req = notify_req.header("Authorization", format!("Bearer {t}"));
        }

        notify_req.send().await?;

        Ok(Self {
            http,
            base_url,
            session_id,
            next_id: 10,
            auth_token: token.map(String::from),
        })
    }

    /// Auto-discover a running Victauri server via temp files.
    ///
    /// Reads `<temp>/victauri.port` and `<temp>/victauri.token` written by the
    /// plugin on startup. Falls back to `VICTAURI_PORT` / `VICTAURI_AUTH_TOKEN`
    /// env vars, then defaults (port 7373, no auth).
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Connection`] if the server is unreachable or
    /// returns a non-success status. Returns [`TestError::Request`] on
    /// HTTP transport failures.
    pub async fn discover() -> Result<Self, TestError> {
        let port = Self::discover_port();
        let token = Self::discover_token();
        Self::connect_with_token(port, token.as_deref()).await
    }

    fn discover_port() -> u16 {
        if let Ok(p) = std::env::var("VICTAURI_PORT")
            && let Ok(port) = p.parse::<u16>()
        {
            return port;
        }
        let path = std::env::temp_dir().join("victauri.port");
        if let Ok(contents) = std::fs::read_to_string(&path)
            && let Ok(port) = contents.trim().parse::<u16>()
        {
            return port;
        }
        7373
    }

    fn discover_token() -> Option<String> {
        if let Ok(token) = std::env::var("VICTAURI_AUTH_TOKEN") {
            return Some(token);
        }
        let path = std::env::temp_dir().join("victauri.token");
        let token = std::fs::read_to_string(&path).ok()?;
        let token = token.trim().to_string();
        if token.is_empty() { None } else { Some(token) }
    }

    /// Call an MCP tool by name and return the result content as JSON.
    ///
    /// Retries up to 3 times with exponential backoff on 429 (rate limited).
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Connection`] if the request fails after retries.
    /// Returns [`TestError::Request`] on HTTP transport errors.
    /// Returns [`TestError::Mcp`] if the server returns a JSON-RPC error.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, TestError> {
        let id = self.next_id;
        self.next_id += 1;

        let call_body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        });

        let mut resp = None;
        for attempt in 0..4 {
            let mut req = self
                .http
                .post(format!("{}/mcp", self.base_url))
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .header("mcp-session-id", &self.session_id)
                .json(&call_body);
            if let Some(ref t) = self.auth_token {
                req = req.header("Authorization", format!("Bearer {t}"));
            }
            let r = req.send().await?;

            if r.status() == 429 && attempt < 3 {
                let delay = std::time::Duration::from_millis(100 * (1 << attempt));
                tokio::time::sleep(delay).await;
                continue;
            }
            resp = Some(r);
            break;
        }

        let resp =
            resp.ok_or_else(|| TestError::Connection("tool call failed after retries".into()))?;
        let body = Self::parse_response(resp).await?;

        if let Some(error) = body.get("error") {
            return Err(TestError::Mcp {
                code: error["code"].as_i64().unwrap_or(-1),
                message: error["message"].as_str().unwrap_or("unknown").to_string(),
            });
        }

        let content = &body["result"]["content"];
        if let Some(arr) = content.as_array()
            && let Some(first) = arr.first()
            && let Some(text) = first["text"].as_str()
        {
            if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                return Ok(parsed);
            }
            return Ok(Value::String(text.to_string()));
        }

        Ok(body)
    }

    /// Parse a response that may be JSON or SSE (text/event-stream).
    ///
    /// rmcp's Streamable HTTP transport always returns SSE format with the
    /// JSON-RPC payload in a `data:` line. This method handles both formats.
    async fn parse_response(resp: reqwest::Response) -> Result<Value, TestError> {
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let text = resp.text().await?;

        if content_type.contains("text/event-stream") {
            for line in text.lines() {
                let data = line
                    .strip_prefix("data: ")
                    .or_else(|| line.strip_prefix("data:"));
                let Some(data) = data else { continue };
                let trimmed = data.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                    return Ok(parsed);
                }
            }
            Err(TestError::Connection(
                "SSE stream contained no JSON-RPC data".into(),
            ))
        } else {
            serde_json::from_str(&text).map_err(|e| {
                TestError::Connection(format!(
                    "JSON parse error: {e}, body: {}",
                    &text[..200.min(text.len())]
                ))
            })
        }
    }

    /// Evaluate JavaScript in the webview and return the result.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn eval_js(&mut self, code: &str) -> Result<Value, TestError> {
        self.call_tool("eval_js", json!({"code": code})).await
    }

    /// Get a DOM snapshot of the current page.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn dom_snapshot(&mut self) -> Result<Value, TestError> {
        self.call_tool("dom_snapshot", json!({})).await
    }

    /// Click an element by ref handle ID.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn click(&mut self, ref_id: &str) -> Result<Value, TestError> {
        self.call_tool("interact", json!({"action": "click", "ref_id": ref_id}))
            .await
    }

    /// Fill an input element with a value.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn fill(&mut self, ref_id: &str, value: &str) -> Result<Value, TestError> {
        self.call_tool(
            "input",
            json!({"action": "fill", "ref_id": ref_id, "value": value}),
        )
        .await
    }

    /// Type text into an element character by character.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn type_text(&mut self, ref_id: &str, text: &str) -> Result<Value, TestError> {
        self.call_tool(
            "input",
            json!({"action": "type_text", "ref_id": ref_id, "text": text}),
        )
        .await
    }

    /// List all window labels.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn list_windows(&mut self) -> Result<Value, TestError> {
        self.call_tool("window", json!({"action": "list"})).await
    }

    /// Get the state of a specific window (or all windows).
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn get_window_state(&mut self, label: Option<&str>) -> Result<Value, TestError> {
        let mut args = json!({"action": "get_state"});
        if let Some(l) = label {
            args["label"] = json!(l);
        }
        self.call_tool("window", args).await
    }

    /// Take a screenshot and return base64-encoded PNG.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn screenshot(&mut self) -> Result<Value, TestError> {
        self.call_tool("screenshot", json!({})).await
    }

    /// Take a screenshot and compare it against a stored baseline.
    ///
    /// Captures the current window, extracts the base64 PNG data, and passes
    /// it to [`visual::compare_screenshot`](crate::visual::compare_screenshot).
    /// On first run the screenshot is saved as the new baseline.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::VisualRegression`] if the diff exceeds the
    /// threshold, or [`TestError::Other`] if the screenshot result does not
    /// contain recognizable image data.
    pub async fn screenshot_visual(
        &mut self,
        name: &str,
        options: &VisualOptions,
    ) -> Result<VisualDiff, TestError> {
        let result = self.screenshot().await?;
        let base64_data = extract_screenshot_base64(&result)?;
        crate::visual::compare_screenshot(name, &base64_data, options)
    }

    /// Invoke a Tauri command by name with optional arguments.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn invoke_command(
        &mut self,
        command: &str,
        args: Option<Value>,
    ) -> Result<Value, TestError> {
        let mut params = json!({"command": command});
        if let Some(a) = args {
            params["args"] = a;
        }
        self.call_tool("invoke_command", params).await
    }

    /// Get the IPC call log.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn get_ipc_log(&mut self, limit: Option<usize>) -> Result<Value, TestError> {
        let mut args = json!({"action": "ipc"});
        if let Some(n) = limit {
            args["limit"] = json!(n);
        }
        self.call_tool("logs", args).await
    }

    /// Verify frontend state against backend state.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn verify_state(
        &mut self,
        frontend_expr: &str,
        backend_state: Value,
    ) -> Result<Value, TestError> {
        self.call_tool(
            "verify_state",
            json!({
                "frontend_expr": frontend_expr,
                "backend_state": backend_state,
            }),
        )
        .await
    }

    /// Detect ghost commands (registered but never called, or called but not registered).
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn detect_ghost_commands(&mut self) -> Result<Value, TestError> {
        self.call_tool("detect_ghost_commands", json!({})).await
    }

    /// Check IPC call health (pending, stale, errored).
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn check_ipc_integrity(&mut self) -> Result<Value, TestError> {
        self.call_tool("check_ipc_integrity", json!({})).await
    }

    /// Run a semantic assertion against a JS expression.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn assert_semantic(
        &mut self,
        expression: &str,
        label: &str,
        condition: &str,
        expected: Value,
    ) -> Result<Value, TestError> {
        self.call_tool(
            "assert_semantic",
            json!({
                "expression": expression,
                "label": label,
                "condition": condition,
                "expected": expected,
            }),
        )
        .await
    }

    /// Run an accessibility audit.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn audit_accessibility(&mut self) -> Result<Value, TestError> {
        self.call_tool("inspect", json!({"action": "audit_accessibility"}))
            .await
    }

    /// Get performance metrics (timing, heap, resources).
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn get_performance_metrics(&mut self) -> Result<Value, TestError> {
        self.call_tool("inspect", json!({"action": "get_performance"}))
            .await
    }

    /// Get the command registry.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn get_registry(&mut self) -> Result<Value, TestError> {
        self.call_tool("get_registry", json!({})).await
    }

    /// Get process memory statistics.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn get_memory_stats(&mut self) -> Result<Value, TestError> {
        self.call_tool("get_memory_stats", json!({})).await
    }

    /// Read plugin info (version, uptime, tool count).
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn get_plugin_info(&mut self) -> Result<Value, TestError> {
        self.call_tool("get_plugin_info", json!({})).await
    }

    /// Wait for a condition to be met, polling at an interval.
    ///
    /// Conditions: `text`, `text_gone`, `selector`, `selector_gone`, `url`,
    /// `ipc_idle`, `network_idle`.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn wait_for(
        &mut self,
        condition: &str,
        value: Option<&str>,
        timeout_ms: Option<u64>,
        poll_ms: Option<u64>,
    ) -> Result<Value, TestError> {
        let mut args = json!({"condition": condition});
        if let Some(v) = value {
            args["value"] = json!(v);
        }
        if let Some(t) = timeout_ms {
            args["timeout_ms"] = json!(t);
        }
        if let Some(p) = poll_ms {
            args["poll_ms"] = json!(p);
        }
        self.call_tool("wait_for", args).await
    }

    /// Start a time-travel recording session.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn start_recording(&mut self, session_id: Option<&str>) -> Result<Value, TestError> {
        let mut args = json!({"action": "start"});
        if let Some(id) = session_id {
            args["session_id"] = json!(id);
        }
        self.call_tool("recording", args).await
    }

    /// Stop the recording and return the session.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn stop_recording(&mut self) -> Result<Value, TestError> {
        self.call_tool("recording", json!({"action": "stop"})).await
    }

    /// Export the current recording session as JSON.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn export_session(&mut self) -> Result<Value, TestError> {
        self.call_tool("recording", json!({"action": "export"}))
            .await
    }

    /// Search for elements by various criteria without a full snapshot.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn find_elements(&mut self, query: Value) -> Result<Value, TestError> {
        self.call_tool("find_elements", query).await
    }

    /// Hover over an element by ref handle.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn hover(&mut self, ref_id: &str) -> Result<Value, TestError> {
        self.call_tool("interact", json!({"action": "hover", "ref_id": ref_id}))
            .await
    }

    /// Focus an element by ref handle.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn focus(&mut self, ref_id: &str) -> Result<Value, TestError> {
        self.call_tool("interact", json!({"action": "focus", "ref_id": ref_id}))
            .await
    }

    /// Press a keyboard key.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn press_key(&mut self, key: &str) -> Result<Value, TestError> {
        self.call_tool("input", json!({"action": "press_key", "key": key}))
            .await
    }

    /// Navigate to a URL.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn navigate(&mut self, url: &str) -> Result<Value, TestError> {
        self.call_tool("navigate", json!({"action": "go_to", "url": url}))
            .await
    }

    /// Get logs by type (console, network, ipc, navigation, dialogs).
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn logs(&mut self, action: &str, limit: Option<usize>) -> Result<Value, TestError> {
        self.call_tool("logs", json!({"action": action, "limit": limit}))
            .await
    }

    /// Scroll an element into view by ref handle.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn scroll_to(&mut self, ref_id: &str) -> Result<Value, TestError> {
        self.call_tool(
            "interact",
            json!({"action": "scroll_into_view", "ref_id": ref_id}),
        )
        .await
    }

    /// Select option(s) in a `<select>` element.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn select_option(
        &mut self,
        ref_id: &str,
        values: &[&str],
    ) -> Result<Value, TestError> {
        self.call_tool(
            "interact",
            json!({"action": "select_option", "ref_id": ref_id, "values": values}),
        )
        .await
    }

    /// Get the server base URL.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Get the MCP session ID.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    // ── IPC Log Helpers ───────────────────────────────────────────────────────

    /// Get IPC calls filtered to a specific command.
    ///
    /// Returns a Vec of all IPC log entries matching the given command name.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn get_ipc_calls(&mut self, command: &str) -> Result<Vec<Value>, TestError> {
        let log = self.get_ipc_log(None).await?;
        let entries = if let Some(arr) = log.as_array() {
            arr.clone()
        } else if let Some(entries) = log.get("entries").and_then(Value::as_array) {
            entries.clone()
        } else {
            return Ok(Vec::new());
        };
        Ok(entries
            .into_iter()
            .filter(|e| {
                e.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|c| c == command)
            })
            .collect())
    }

    /// Snapshot the current IPC log length, for use with `ipc_calls_since`.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn ipc_checkpoint(&mut self) -> Result<usize, TestError> {
        let log = self.get_ipc_log(None).await?;
        let len = if let Some(arr) = log.as_array() {
            arr.len()
        } else if let Some(entries) = log.get("entries").and_then(Value::as_array) {
            entries.len()
        } else {
            0
        };
        Ok(len)
    }

    /// Get IPC calls made since a previous checkpoint.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn ipc_calls_since(&mut self, checkpoint: usize) -> Result<Vec<Value>, TestError> {
        let log = self.get_ipc_log(None).await?;
        let entries = if let Some(arr) = log.as_array() {
            arr.clone()
        } else if let Some(entries) = log.get("entries").and_then(Value::as_array) {
            entries.clone()
        } else {
            return Ok(Vec::new());
        };
        Ok(entries.into_iter().skip(checkpoint).collect())
    }

    // ── Fluent Verification Builder ───────────────────────────────────────────

    /// Start a fluent verification chain that checks multiple conditions at once.
    ///
    /// Unlike individual assertions that panic on failure, `verify()` collects
    /// all results and reports them together — making test failures more
    /// informative and reducing test reruns.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// let report = client.verify()
    ///     .has_text("Welcome")
    ///     .ipc_was_called("greet")
    ///     .no_console_errors()
    ///     .run()
    ///     .await
    ///     .unwrap();
    /// report.assert_all_passed();
    /// ```
    pub fn verify(&mut self) -> VerifyBuilder<'_> {
        VerifyBuilder::new(self)
    }

    // ── High-Level Playwright-Style API ─────────────────────────────────────

    /// Click the first element whose accessible text contains the given string.
    ///
    /// Takes a DOM snapshot, finds the element, and clicks it.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no matching element is found.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn click_by_text(&mut self, text: &str) -> Result<Value, TestError> {
        let ref_id = self.find_ref_by_text(text).await?;
        self.click(&ref_id).await
    }

    /// Click the element with the given HTML `id` attribute.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element has the given id.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn click_by_id(&mut self, id: &str) -> Result<Value, TestError> {
        let ref_id = self.find_ref_by_id(id).await?;
        self.click(&ref_id).await
    }

    /// Fill an input identified by HTML `id` with the given value.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element has the given id.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn fill_by_id(&mut self, id: &str, value: &str) -> Result<Value, TestError> {
        let ref_id = self.find_ref_by_id(id).await?;
        self.fill(&ref_id, value).await
    }

    /// Type text into an input identified by HTML `id`, character by character.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element has the given id.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn type_by_id(&mut self, id: &str, text: &str) -> Result<Value, TestError> {
        let ref_id = self.find_ref_by_id(id).await?;
        self.type_text(&ref_id, text).await
    }

    /// Wait until the page contains the given text (polls DOM snapshots).
    ///
    /// Default timeout: 5000ms, poll interval: 200ms.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the text doesn't appear within the timeout.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn expect_text(&mut self, text: &str) -> Result<(), TestError> {
        self.expect_text_with_timeout(text, 5000).await
    }

    /// Wait until the page contains the given text, with a custom timeout in ms.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the text doesn't appear within the timeout.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn expect_text_with_timeout(
        &mut self,
        text: &str,
        timeout_ms: u64,
    ) -> Result<(), TestError> {
        let result = self
            .wait_for("text", Some(text), Some(timeout_ms), Some(200))
            .await?;
        if result.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err(TestError::Timeout(format!(
                "text \"{text}\" did not appear within {timeout_ms}ms"
            )))
        }
    }

    /// Wait until the page no longer contains the given text.
    ///
    /// Default timeout: 3000ms, poll interval: 200ms.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Timeout`] if the text is still present after the timeout.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn expect_no_text(&mut self, text: &str) -> Result<(), TestError> {
        let result = self
            .wait_for("text_gone", Some(text), Some(3000), Some(200))
            .await?;
        if result.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err(TestError::Timeout(format!(
                "text \"{text}\" still present after 3000ms"
            )))
        }
    }

    /// Select an option in a `<select>` element identified by HTML `id`.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element has the given id.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn select_by_id(&mut self, id: &str, value: &str) -> Result<Value, TestError> {
        let ref_id = self.find_ref_by_id(id).await?;
        self.select_option(&ref_id, &[value]).await
    }

    /// Get the text content of an element identified by HTML `id`.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element has the given id.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn text_by_id(&mut self, id: &str) -> Result<String, TestError> {
        let snap = self.snapshot_json().await?;
        let tree = &snap["tree"];
        find_text_by_attr_id(tree, id)
            .ok_or_else(|| TestError::ElementNotFound(format!("id=\"{id}\"")))
    }

    // ── Internal helpers for high-level API ─────────────────────────────────

    async fn snapshot_json(&mut self) -> Result<Value, TestError> {
        self.call_tool("dom_snapshot", json!({"format": "json"}))
            .await
    }

    async fn find_ref_by_text(&mut self, text: &str) -> Result<String, TestError> {
        let snap = self.snapshot_json().await?;
        let tree = &snap["tree"];
        find_in_tree_by_text(tree, text)
            .ok_or_else(|| TestError::ElementNotFound(format!("text=\"{text}\"")))
    }

    async fn find_ref_by_id(&mut self, id: &str) -> Result<String, TestError> {
        let snap = self.snapshot_json().await?;
        let tree = &snap["tree"];
        find_in_tree_by_attr_id(tree, id)
            .ok_or_else(|| TestError::ElementNotFound(format!("id=\"{id}\"")))
    }
}

fn extract_screenshot_base64(result: &Value) -> Result<String, TestError> {
    // Try various response shapes the plugin may return
    if let Some(data) = result.get("base64").and_then(Value::as_str) {
        return Ok(data.to_string());
    }
    if let Some(data) = result.get("data").and_then(Value::as_str) {
        return Ok(data.to_string());
    }
    if let Some(data) = result.get("image").and_then(Value::as_str) {
        return Ok(data.to_string());
    }
    if let Some(data) = result.pointer("/result/content/0/data").and_then(Value::as_str) {
        return Ok(data.to_string());
    }
    Err(TestError::Other(
        "screenshot result does not contain recognizable base64 image data".to_string(),
    ))
}

fn find_in_tree_by_text(node: &Value, text: &str) -> Option<String> {
    let node_text = node.get("text").and_then(Value::as_str).unwrap_or("");
    let node_name = node.get("name").and_then(Value::as_str).unwrap_or("");
    if (node_text.contains(text) || node_name.contains(text))
        && let Some(ref_id) = node.get("ref_id").and_then(Value::as_str)
    {
        return Some(ref_id.to_string());
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            if let Some(found) = find_in_tree_by_text(child, text) {
                return Some(found);
            }
        }
    }
    None
}

fn find_in_tree_by_attr_id(node: &Value, id: &str) -> Option<String> {
    if node
        .get("attributes")
        .and_then(|a| a.get("id"))
        .and_then(Value::as_str)
        == Some(id)
        && let Some(ref_id) = node.get("ref_id").and_then(Value::as_str)
    {
        return Some(ref_id.to_string());
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            if let Some(found) = find_in_tree_by_attr_id(child, id) {
                return Some(found);
            }
        }
    }
    None
}

fn find_text_by_attr_id(node: &Value, id: &str) -> Option<String> {
    if node
        .get("attributes")
        .and_then(|a| a.get("id"))
        .and_then(Value::as_str)
        == Some(id)
    {
        let text = node.get("text").and_then(Value::as_str).unwrap_or("");
        return Some(text.to_string());
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            if let Some(found) = find_text_by_attr_id(child, id) {
                return Some(found);
            }
        }
    }
    None
}

// ── Assertion Helpers ────────────────────────────────────────────────────────

/// Assert that a JSON value at the given pointer equals the expected value.
///
/// # Panics
///
/// Panics if the value at `pointer` is missing or does not equal `expected`.
///
/// # Examples
///
/// ```
/// use serde_json::json;
///
/// let state = json!({"visible": true, "title": "My App"});
/// victauri_test::assert_json_eq(&state, "/visible", &json!(true));
/// victauri_test::assert_json_eq(&state, "/title", &json!("My App"));
/// ```
pub fn assert_json_eq(value: &Value, pointer: &str, expected: &Value) {
    let actual = value.pointer(pointer);
    assert!(
        actual == Some(expected),
        "JSON pointer {pointer}: expected {expected}, got {}",
        actual.map_or("missing".to_string(), std::string::ToString::to_string)
    );
}

/// Assert that a JSON value at the given pointer is truthy (not null/false/0/"").
///
/// # Panics
///
/// Panics if the value at `pointer` is missing, null, false, zero, or empty.
///
/// # Examples
///
/// ```
/// use serde_json::json;
///
/// let value = json!({"active": true, "name": "test", "count": 42});
/// victauri_test::assert_json_truthy(&value, "/active");
/// victauri_test::assert_json_truthy(&value, "/name");
/// victauri_test::assert_json_truthy(&value, "/count");
/// ```
pub fn assert_json_truthy(value: &Value, pointer: &str) {
    let actual = value.pointer(pointer);
    let is_truthy = match actual {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0) != 0.0,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(_)) => true,
    };
    assert!(
        is_truthy,
        "JSON pointer {pointer}: expected truthy, got {}",
        actual.map_or("missing".to_string(), std::string::ToString::to_string)
    );
}

/// Assert that an accessibility audit has zero violations.
///
/// # Panics
///
/// Panics if the audit contains any violations.
///
/// # Examples
///
/// ```
/// use serde_json::json;
///
/// let audit = json!({"summary": {"violations": 0, "passes": 12}});
/// victauri_test::assert_no_a11y_violations(&audit);
/// ```
pub fn assert_no_a11y_violations(audit: &Value) {
    let violations = audit
        .pointer("/summary/violations")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(u64::MAX);
    assert_eq!(
        violations, 0,
        "expected 0 accessibility violations, got {violations}"
    );
}

/// Assert that all performance metrics are within budget.
///
/// # Panics
///
/// Panics if load time exceeds `max_load_ms` or heap usage exceeds `max_heap_mb`.
///
/// # Examples
///
/// ```
/// use serde_json::json;
///
/// let metrics = json!({
///     "navigation": {"load_event_ms": 450.0},
///     "js_heap": {"used_mb": 12.5}
/// });
/// victauri_test::assert_performance_budget(&metrics, 1000.0, 50.0);
/// ```
pub fn assert_performance_budget(metrics: &Value, max_load_ms: f64, max_heap_mb: f64) {
    if let Some(load) = metrics
        .pointer("/navigation/load_event_ms")
        .and_then(serde_json::Value::as_f64)
    {
        assert!(
            load <= max_load_ms,
            "load event took {load}ms, budget is {max_load_ms}ms"
        );
    }

    if let Some(heap) = metrics
        .pointer("/js_heap/used_mb")
        .and_then(serde_json::Value::as_f64)
    {
        assert!(
            heap <= max_heap_mb,
            "JS heap is {heap}MB, budget is {max_heap_mb}MB"
        );
    }
}

/// Assert that IPC integrity is healthy (no stale or errored calls).
///
/// # Panics
///
/// Panics if the integrity check reports an unhealthy state.
///
/// # Examples
///
/// ```
/// use serde_json::json;
///
/// let integrity = json!({"healthy": true, "stale_calls": 0, "error_calls": 0});
/// victauri_test::assert_ipc_healthy(&integrity);
/// ```
pub fn assert_ipc_healthy(integrity: &Value) {
    let healthy = integrity
        .get("healthy")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    assert!(
        healthy,
        "IPC integrity check failed: {}",
        serde_json::to_string_pretty(integrity).unwrap_or_default()
    );
}

/// Assert that state verification passed with no divergences.
///
/// # Panics
///
/// Panics if the verification reports any divergences.
///
/// # Examples
///
/// ```
/// use serde_json::json;
///
/// let verification = json!({"passed": true, "divergences": []});
/// victauri_test::assert_state_matches(&verification);
/// ```
pub fn assert_state_matches(verification: &Value) {
    let passed = verification
        .get("passed")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    assert!(
        passed,
        "state verification failed: {}",
        serde_json::to_string_pretty(verification).unwrap_or_default()
    );
}
