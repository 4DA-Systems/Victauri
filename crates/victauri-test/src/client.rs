use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::assertions::VerifyBuilder;
use crate::error::TestError;
use crate::visual::{VisualDiff, VisualOptions};

const IPC_CHECKPOINT_CLOCK_ADVANCE_CAP: Duration = Duration::from_millis(5);

fn current_epoch_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

async fn wait_past_ipc_checkpoint_ms(checkpoint_ms: u64) {
    let Some(now_ms) = current_epoch_ms() else {
        return;
    };
    if checkpoint_ms == 0 || checkpoint_ms < now_ms {
        return;
    }

    let sleep_ms = checkpoint_ms.saturating_sub(now_ms).saturating_add(1);
    let cap_ms = u64::try_from(IPC_CHECKPOINT_CLOCK_ADVANCE_CAP.as_millis()).unwrap_or(u64::MAX);
    tokio::time::sleep(Duration::from_millis(sleep_ms.min(cap_ms))).await;
}

/// Default whole-request HTTP timeout for MCP calls.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Headroom added on top of a tool's own `timeout_ms` so the HTTP request never
/// times out before the server-side wait it is waiting on has finished. Before that wait
/// starts the server may wait for the webview bridge (up to 5 s), probe it (up to 2 s) and
/// make main-thread round trips — the same allowance `victauri bridge` uses.
const TOOL_TIMEOUT_HEADROOM: Duration = Duration::from_secs(40);

/// The server's ceiling for any per-call `timeout_ms` (`invoke_command`).
const MAX_TOOL_TIMEOUT_MS: u64 = 300_000;

/// Per-request HTTP timeout for a tool call. Tools that block server-side for a
/// caller-chosen `timeout_ms` (e.g. `wait_for`, which accepts up to 120 000 ms)
/// need the HTTP timeout to outlast that wait; otherwise the client gives up at
/// the fixed 60 s default while the server is still legitimately waiting.
///
/// A tool call WITHOUT `timeout_ms` is bounded server-side by the app's configured eval timeout,
/// which can be up to 300 s; the client must not give up first, or a retry would run a
/// side-effecting call twice (round-4 review — the same class `victauri bridge` fixed). The
/// server still answers as soon as the call finishes, so this never slows a normal call.
fn request_timeout_for(arguments: &Value) -> Duration {
    let ms = arguments
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(MAX_TOOL_TIMEOUT_MS);
    DEFAULT_REQUEST_TIMEOUT.max(
        Duration::from_millis(ms.min(MAX_TOOL_TIMEOUT_MS)).saturating_add(TOOL_TIMEOUT_HEADROOM),
    )
}

/// Whether an HTTP status from `GET /health` proves the Victauri server is alive.
///
/// A `2xx` does, and so does `429 Too Many Requests`: `/health` shares the public rate-limit
/// bucket, so anyone on the machine can flood it, and a rate-limited reply is still the
/// server answering. Treating 429 as "down" let an unauthenticated flood make every client
/// report a live app as unreachable (round-4 audit R4-NET1).
#[doc(hidden)]
#[must_use]
pub fn health_status_means_alive(status: u16) -> bool {
    (200..300).contains(&status) || status == 429
}

/// Truncate `text` to at most `max_chars` characters without splitting a UTF-8
/// code point (byte slicing like `&text[..200]` panics on non-ASCII bodies).
pub fn truncate_chars(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &text[..byte_idx],
        None => text,
    }
}

/// Split an SSE body into event payloads (the `data:` lines of each event joined
/// with `\n`, per the SSE spec), skipping events with no data (e.g. priming events).
fn sse_event_payloads(text: &str) -> Vec<String> {
    let mut events = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if !current.is_empty() {
                events.push(current.join("\n"));
                current.clear();
            }
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            current.push(data.strip_prefix(' ').unwrap_or(data));
        }
    }
    if !current.is_empty() {
        events.push(current.join("\n"));
    }
    events
}

/// Pick the JSON-RPC response whose `id` matches `expected_id` out of an SSE
/// body. A stream may carry server notifications/requests (no matching `id`)
/// before the response, so "first data line" is not a safe choice.
fn find_sse_response(text: &str, expected_id: u64) -> Option<Value> {
    sse_event_payloads(text)
        .into_iter()
        .filter_map(|payload| serde_json::from_str::<Value>(payload.trim()).ok())
        .find(|msg| {
            msg.get("id").and_then(Value::as_u64) == Some(expected_id)
                && (msg.get("result").is_some() || msg.get("error").is_some())
        })
}

// ── Typed Response Structs (Phase 4E) ───────────────────────────────────────

/// Structured plugin information returned by [`VictauriClient::plugin_info`].
///
/// # Example
///
/// ```rust,ignore
/// let info = client.plugin_info().await.unwrap();
/// println!("v{} — {} tools, up {:.0}s", info.version, info.tools.total, info.uptime_secs);
/// ```
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct PluginInfo {
    /// Plugin version string (e.g. `"0.2.0"`).
    pub version: String,
    /// Seconds since the plugin was initialized.
    pub uptime_secs: f64,
    /// Total number of tool invocations served.
    pub tool_invocations: u64,
    /// Tool details (nested object with `total`, `enabled`, etc.).
    #[serde(default)]
    pub tools: PluginToolInfo,
    /// Number of tools — derived from `tools.total` if present, else top-level
    /// `tool_count` for backwards compatibility.
    #[serde(default)]
    pub tool_count: usize,
}

/// Tool information nested inside [`PluginInfo`].
#[derive(Debug, Clone, Default, Deserialize)]
#[non_exhaustive]
pub struct PluginToolInfo {
    /// Total number of tools registered.
    #[serde(default)]
    pub total: usize,
    /// Number of enabled tools.
    #[serde(default)]
    pub enabled: usize,
}

/// Structured process memory statistics returned by [`VictauriClient::memory_stats`].
///
/// # Example
///
/// ```rust,ignore
/// let mem = client.memory_stats().await.unwrap();
/// let mb = mem.working_set_bytes as f64 / 1_048_576.0;
/// assert!(mb < 512.0, "memory usage too high: {mb:.1} MB");
/// ```
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct MemoryStats {
    /// Current working set size in bytes.
    pub working_set_bytes: u64,
    /// Peak working set size in bytes, if available.
    pub peak_working_set_bytes: Option<u64>,
}

// ── WaitForBuilder (Phase 4B) ───────────────────────────────────────────────

/// Builder for configuring and executing a `wait_for` condition.
///
/// Created via [`VictauriClient::wait`]. Provides a fluent API as an
/// alternative to the positional-argument [`VictauriClient::wait_for`] method.
///
/// # Examples
///
/// ```rust,ignore
/// // Wait for text to appear with custom timeout
/// client.wait("text")
///     .value("Hello, World!")
///     .timeout_ms(15_000)
///     .run()
///     .await
///     .unwrap();
///
/// // Wait for network idle with fast polling
/// client.wait("network_idle")
///     .poll_ms(50)
///     .run()
///     .await
///     .unwrap();
/// ```
pub struct WaitForBuilder<'a> {
    client: &'a mut VictauriClient,
    condition: String,
    value: Option<String>,
    timeout_ms: u64,
    poll_ms: u64,
}

impl<'a> WaitForBuilder<'a> {
    /// Set the value to match against (e.g. the text string for `"text"` condition).
    #[must_use]
    pub fn value(mut self, v: &str) -> Self {
        self.value = Some(v.to_string());
        self
    }

    /// Set the maximum time to wait in milliseconds (default: 10 000).
    #[must_use]
    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = ms;
        self
    }

    /// Set the polling interval in milliseconds (default: 200).
    #[must_use]
    pub fn poll_ms(mut self, ms: u64) -> Self {
        self.poll_ms = ms;
        self
    }

    /// Execute the wait, polling until the condition is met or the timeout expires.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn run(self) -> Result<Value, TestError> {
        self.client
            .wait_for(
                &self.condition,
                self.value.as_deref(),
                Some(self.timeout_ms),
                Some(self.poll_ms),
            )
            .await
    }
}

/// Typed HTTP client for the Victauri MCP server.
///
/// Runs the MCP handshake (initialize → tool calls) and provides convenient
/// methods for common test operations. The server's default transport is
/// stateless, so no session is minted or tracked.
///
/// # Example
///
/// ```rust,ignore
/// use victauri_test::VictauriClient;
///
/// let mut client = VictauriClient::connect(7373).await.unwrap();
/// let title = client.eval_js("document.title").await.unwrap();
/// client.click("e3").await.unwrap();
/// let snapshot = client.dom_snapshot().await.unwrap();
/// ```
pub struct VictauriClient {
    http: reqwest::Client,
    base_url: String,
    host: String,
    port: u16,
    /// MCP session id echoed on later calls. In *stateful* mode `initialize` mints a unique id. In
    /// *stateless* mode (the Victauri default since the 422-wedge fix) the server backfills a
    /// constant compat id (`"stateless"`) so old/strict clients don't abort on a missing header;
    /// since it is never validated, echoing it can never trigger the stale-session 422 path. `None`
    /// only if a non-default server emits no header at all.
    session_id: Option<String>,
    next_id: u64,
    auth_token: Option<String>,
    /// `true` when the endpoint came from discovery ([`Self::discover`]). The plugin
    /// mints a fresh auth token on every launch and may land on a different port, so
    /// such a client re-resolves port + token from discovery when it has to
    /// re-handshake instead of replaying stale credentials. `false` for explicit
    /// [`Self::connect_with_token`] clients, whose endpoint is the caller's choice.
    discovered: bool,
    /// Tauri app identifier of the server this client connected to, recorded at
    /// [`Self::discover`] time (from `/info`, else discovery metadata). Rediscovery
    /// only accepts a server with this same identity, so a restarted client can never
    /// silently bind to a different app that took over the port.
    app_identifier: Option<String>,
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
        let host = "127.0.0.1";
        let base_url = format!("http://{host}:{port}");
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| TestError::Connection {
                host: host.to_string(),
                port,
                reason: e.to_string(),
            })?;

        let session_id = Self::perform_handshake(&http, &base_url, host, port, token).await?;

        Ok(Self {
            http,
            base_url,
            host: host.to_string(),
            port,
            session_id,
            next_id: 10,
            auth_token: token.map(String::from),
            discovered: false,
            app_identifier: None,
        })
    }

    /// Run the MCP `initialize` + `notifications/initialized` handshake and return the
    /// minted session id, if any. Shared by [`Self::connect_with_token`] and
    /// [`Self::reinitialize`] so a fresh session can be established without rebuilding the
    /// whole client.
    ///
    /// Returns `Ok(Some(id))` when the server emits an `mcp-session-id` header. Stateful mode mints
    /// a unique id; current Victauri stateless mode backfills the constant compat id `"stateless"`.
    /// Returns `Ok(None)` only for servers that emit no session header at all.
    async fn perform_handshake(
        http: &reqwest::Client,
        base_url: &str,
        host: &str,
        port: u16,
        token: Option<&str>,
    ) -> Result<Option<String>, TestError> {
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

            let resp = req.send().await.map_err(|e| TestError::Connection {
                host: host.to_string(),
                port,
                reason: e.to_string(),
            })?;

            if resp.status() == 429 && attempt < 3 {
                let delay = std::time::Duration::from_millis(100 * (1 << attempt));
                tokio::time::sleep(delay).await;
                continue;
            }

            init_resp = Some(resp);
            break;
        }

        let init_resp = init_resp.ok_or_else(|| TestError::Connection {
            host: host.to_string(),
            port,
            reason: "initialize failed after retries".into(),
        })?;

        if !init_resp.status().is_success() {
            return Err(TestError::Connection {
                host: host.to_string(),
                port,
                reason: format!("initialize returned {}", init_resp.status()),
            });
        }

        // Echo a returned `mcp-session-id` on later calls. In stateful mode it is the actual
        // per-client session id; in Victauri's stateless default it is the constant compat id
        // "stateless", which the server deliberately ignores.
        let session_id = init_resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        let mut notify_req = http
            .post(format!("{base_url}/mcp"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(&json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }));

        if let Some(ref sid) = session_id {
            notify_req = notify_req.header("mcp-session-id", sid);
        }
        if let Some(t) = token {
            notify_req = notify_req.header("Authorization", format!("Bearer {t}"));
        }

        notify_req.send().await?;

        Ok(session_id)
    }

    /// Re-run the MCP handshake to mint a fresh session, replacing the stale one.
    ///
    /// Called automatically by [`Self::call_tool`] when a tool call returns HTTP 422
    /// "expected initialized request" — the session went stale because the in-app server
    /// restarted, the client reconnected, or the `notifications/initialized` was missed.
    async fn reinitialize(&mut self) -> Result<(), TestError> {
        if self.discovered {
            self.rediscover()?;
        }
        let token = self.auth_token.clone();
        let session_id = Self::perform_handshake(
            &self.http,
            &self.base_url,
            &self.host,
            self.port,
            token.as_deref(),
        )
        .await?;
        self.session_id = session_id;
        if self.discovered {
            self.verify_identity().await?;
        }
        Ok(())
    }

    /// Re-resolve the endpoint (port + auth token) from discovery for the SAME app
    /// [`Self::discover`] connected to. Only meaningful for discovered clients: after
    /// an app restart the plugin has a fresh token and possibly a different port.
    ///
    /// Errors (leaving the endpoint unchanged) when no live server with the recorded
    /// app identity is found — it never falls back to the default port.
    fn rediscover(&mut self) -> Result<(), TestError> {
        let Some((port, token)) =
            crate::discovery::resolve_rediscovery(self.app_identifier.as_deref())
        else {
            return Err(TestError::Connection {
                host: self.host.clone(),
                port: self.port,
                reason: self.app_identifier.as_ref().map_or_else(
                    || {
                        "rediscovery found no unambiguous live Victauri server — the app may \
                         still be restarting"
                            .to_string()
                    },
                    |id| {
                        format!(
                            "rediscovery found no live Victauri server for app '{id}' — the \
                             app may still be restarting (other running apps are ignored)"
                        )
                    },
                ),
            });
        };
        if port != self.port {
            self.port = port;
            self.base_url = format!("http://{}:{port}", self.host);
        }
        self.auth_token = token;
        Ok(())
    }

    /// Read the host app's identifier from the server's `/info` endpoint.
    async fn fetch_app_identifier(&self) -> Option<String> {
        let mut req = self.http.get(format!("{}/info", self.base_url));
        if let Some(ref t) = self.auth_token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        let info: Value = req.send().await.ok()?.json().await.ok()?;
        info.get("app_identifier")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// After a rediscovery re-handshake, confirm the server is still the app this
    /// client was pinned to (defense in depth for explicit `VICTAURI_PORT` configs,
    /// where discovery metadata is not consulted).
    async fn verify_identity(&self) -> Result<(), TestError> {
        let Some(expected) = self.app_identifier.as_deref() else {
            return Ok(());
        };
        match self.fetch_app_identifier().await {
            Some(actual) if actual != expected => Err(TestError::Connection {
                host: self.host.clone(),
                port: self.port,
                reason: format!(
                    "rediscovered server on port {} is app '{actual}', not '{expected}' — \
                     refusing to switch apps",
                    self.port
                ),
            }),
            _ => Ok(()),
        }
    }

    /// The Tauri app identifier this client is pinned to, when known (recorded by
    /// [`Self::discover`]; `None` for [`Self::connect_with_token`] clients).
    #[must_use]
    pub fn app_identifier(&self) -> Option<&str> {
        self.app_identifier.as_deref()
    }

    /// Auto-discover a running Victauri server via temp files.
    ///
    /// Discovery priority:
    /// 1. `VICTAURI_PORT` (+ optional `VICTAURI_AUTH_TOKEN`) — the explicit endpoint
    /// 2. Per-process discovery directories (`<root>/<pid>/{port,token,metadata.json}`),
    ///    narrowed by `VICTAURI_APP` (exact bundle identifier or product name) when set
    /// 3. Default: port 7373, no auth — only when no Victauri app is discovered at all
    ///
    /// A `VICTAURI_AUTH_TOKEN` without `VICTAURI_PORT` is only sent to the running app whose
    /// own discovery token equals it — never to whatever holds the default port.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Other`] when discovery cannot pick one app (several are running
    /// and nothing selects one, the selected app is not running, or an explicit token
    /// matches no running app) — the message names the running apps and how to select one.
    /// Returns [`TestError::Connection`] if the server is unreachable or returns a
    /// non-success status. Returns [`TestError::Request`] on HTTP transport failures.
    pub async fn discover() -> Result<Self, TestError> {
        Self::discover_app(None).await
    }

    /// [`Self::discover`], selecting the app by its exact Tauri bundle identifier (or
    /// product name) when several Victauri apps are running. `None` falls back to the
    /// `VICTAURI_APP` env var, then to "the single running app".
    ///
    /// # Errors
    ///
    /// As [`Self::discover`].
    pub async fn discover_app(app: Option<&str>) -> Result<Self, TestError> {
        // Classify the discovery directory BEFORE resolving — resolution deletes the
        // entries of dead apps as a side effect, so the diagnosis must be captured first
        // to explain a subsequent failure.
        let diagnosis = crate::discovery::diagnose_discovery();
        let (port, token) =
            crate::discovery::try_resolve_connection(app).map_err(TestError::Other)?;
        match Self::connect_with_token(port, token.as_deref()).await {
            Ok(mut client) => {
                client.discovered = true;
                // Pin the app identity so a later rediscovery can only re-attach to
                // this same app. `/info` is authoritative (it is the server we just
                // reached); discovery metadata is the fallback for older plugins.
                client.app_identifier = match client.fetch_app_identifier().await {
                    Some(id) => Some(id),
                    None => crate::discovery::identifier_for_port(port),
                };
                Ok(client)
            }
            Err(TestError::Connection { host, port, reason }) => {
                let reason = match diagnosis.hint() {
                    Some(hint) => format!("{reason}\n\n  Discovery diagnosis: {hint}"),
                    None => reason,
                };
                Err(TestError::Connection { host, port, reason })
            }
            Err(other) => Err(other),
        }
    }

    /// Check whether the server is still reachable.
    ///
    /// Sends a GET to `/health` and returns `true` if the server answered with a success
    /// status — or with `429 Too Many Requests`: a rate-limited answer still proves the
    /// server is alive (an unauthenticated `/health` flood must not make a live app look
    /// dead, R4-NET1).
    #[must_use]
    pub async fn is_alive(&self) -> bool {
        self.http
            .get(format!("{}/health", self.base_url))
            .send()
            .await
            .is_ok_and(|r| health_status_means_alive(r.status().as_u16()))
    }

    /// Re-establish an MCP session after the app restarts.
    ///
    /// Polls `/health` up to `max_wait` and then re-runs the
    /// initialize/initialized handshake. The returned client has a fresh
    /// session ID; the old client should be dropped.
    ///
    /// A client created via [`Self::discover`] re-resolves the port and auth
    /// token from discovery on every poll — the restarted app mints a new token
    /// and may bind a different port, so replaying the old ones would 401 (or
    /// wait on a dead port). A [`Self::connect_with_token`] client keeps its
    /// explicit port and token.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Connection`] if the server doesn't come back
    /// within `max_wait`.
    pub async fn reconnect(&self, max_wait: std::time::Duration) -> Result<Self, TestError> {
        let start = std::time::Instant::now();
        loop {
            // A discovered client only re-attaches to its own app (same identifier);
            // no match yet means "not back yet" — never a default-port guess.
            let endpoint = if self.discovered {
                crate::discovery::resolve_rediscovery(self.app_identifier.as_deref())
            } else {
                Some((self.port, self.auth_token.clone()))
            };
            let alive = match &endpoint {
                Some((port, _)) => self
                    .http
                    .get(format!("http://{}:{port}/health", self.host))
                    .send()
                    .await
                    .is_ok_and(|r| health_status_means_alive(r.status().as_u16())),
                None => false,
            };
            if alive && let Some((port, token)) = endpoint {
                let mut client = Self::connect_with_token(port, token.as_deref()).await?;
                client.discovered = self.discovered;
                client.app_identifier.clone_from(&self.app_identifier);
                if client.discovered {
                    client.verify_identity().await?;
                }
                return Ok(client);
            }
            if start.elapsed() > max_wait {
                return Err(TestError::Connection {
                    host: self.host.clone(),
                    port: self.port,
                    reason: format!("server did not recover within {}s", max_wait.as_secs()),
                });
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }

    /// Call an MCP tool by name and return the result content as JSON.
    ///
    /// Retries up to 3 times with exponential backoff on 429 (rate limited). On a 422
    /// "expected initialized request" (the MCP session went stale after an app/server
    /// restart) it re-runs the handshake once and retries transparently; if the session is
    /// still stale, the error names the cause and the sessionless REST fallback.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Connection`] if the request fails after retries (including an
    /// unrecoverable stale session). Returns [`TestError::Request`] on HTTP transport
    /// errors. Returns [`TestError::Mcp`] if the server returns a JSON-RPC error.
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

        let timeout = request_timeout_for(&arguments);
        let mut resp = None;
        let mut reinitialized = false;
        let mut reauthed = false;
        let mut reconnected = false;
        for attempt in 0..4 {
            let mut req = self
                .http
                .post(format!("{}/mcp", self.base_url))
                .timeout(timeout)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .json(&call_body);
            // Echo the server's session header if one was returned. In Victauri's stateless default
            // this is the constant compat id "stateless"; the server ignores it, so it cannot go
            // stale.
            if let Some(ref sid) = self.session_id {
                req = req.header("mcp-session-id", sid);
            }
            if let Some(ref t) = self.auth_token {
                req = req.header("Authorization", format!("Bearer {t}"));
            }
            let r = match req.send().await {
                Ok(r) => r,
                // Connection refused/reset on a discovered client: the app most likely
                // restarted (possibly on another port). Rediscover the SAME app once and
                // retry; explicit-endpoint clients surface the error as-is.
                Err(e) if e.is_connect() && self.discovered && !reconnected && attempt < 3 => {
                    reconnected = true;
                    self.reinitialize().await?;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let status = r.status();

            if status == 429 && attempt < 3 {
                let delay = std::time::Duration::from_millis(100 * (1 << attempt));
                tokio::time::sleep(delay).await;
                continue;
            }
            // HTTP 422 "expected initialized request": the MCP session went stale (the
            // in-app server restarted, the client reconnected, or notifications/initialized
            // was missed). Re-handshake ONCE to mint a fresh session and retry transparently.
            // Bounded by `reinitialized` so a persistently-stale server cannot loop.
            if status == 422 && !reinitialized && attempt < 3 {
                drop(r);
                reinitialized = true;
                self.reinitialize().await?;
                continue;
            }
            // HTTP 401 on a discovered client: the app most likely restarted and minted
            // a fresh token. Re-resolve discovery (new token, maybe new port) and
            // re-handshake once. Explicit-token clients surface the 401 as-is.
            if status == 401 && self.discovered && !reauthed && attempt < 3 {
                drop(r);
                reauthed = true;
                self.reinitialize().await?;
                continue;
            }
            resp = Some(r);
            break;
        }

        let resp = resp.ok_or_else(|| TestError::Connection {
            host: self.host.clone(),
            port: self.port,
            reason: "tool call failed after retries".into(),
        })?;

        // Still stale after a re-handshake: surface a clear, actionable error pointing at
        // the sessionless REST endpoint, which never hits this failure class.
        if resp.status() == 422 {
            return Err(TestError::Connection {
                host: self.host.clone(),
                port: self.port,
                reason: format!(
                    "MCP session is stale (HTTP 422 'expected initialized request') and \
                     re-initialization did not recover — the server likely restarted \
                     mid-session. Use the sessionless REST API instead: \
                     POST http://{}:{}/api/tools/{name} (same Bearer auth, no session \
                     handshake).",
                    self.host, self.port
                ),
            });
        }
        let body = Self::parse_response(resp, &self.host, self.port, id).await?;

        if let Some(error) = body.get("error") {
            return Err(TestError::Mcp {
                code: error["code"].as_i64().unwrap_or(-1),
                message: error["message"].as_str().map_or_else(
                    || {
                        format!(
                            "unknown error (raw: {})",
                            serde_json::to_string(error).unwrap_or_else(|_| "<unparseable>".into())
                        )
                    },
                    String::from,
                ),
            });
        }

        let result = &body["result"];
        let content = &result["content"];

        // Honor MCP tool-level errors: a tool that sets `isError: true` returns its
        // failure message in the content text. Without this check the SDK would
        // report a failed eval / invalid selector / tool_error as a successful
        // `Ok(...)`, hiding real failures from callers (red-team P1).
        let is_tool_error = result.get("isError").and_then(Value::as_bool) == Some(true);

        if let Some(arr) = content.as_array()
            && let Some(first) = arr.first()
        {
            if let Some(text) = first["text"].as_str() {
                if is_tool_error {
                    return Err(TestError::ToolError(text.to_string()));
                }
                if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                    return Ok(parsed);
                }
                return Ok(Value::String(text.to_string()));
            }
            if first.get("type").and_then(Value::as_str) == Some("image") {
                return Ok(first.clone());
            }
        }

        if is_tool_error {
            return Err(TestError::ToolError(format!(
                "tool '{name}' returned an error result: {result}"
            )));
        }

        Ok(body)
    }

    /// Parse a response that may be JSON or SSE (text/event-stream).
    ///
    /// rmcp's Streamable HTTP transport returns SSE with the JSON-RPC payload in
    /// `data:` lines; the response is matched by its JSON-RPC `id` (a stream may
    /// carry notifications first). Non-2xx statuses are reported as such — a 401
    /// or 500 is never misreported as a "JSON parse error".
    async fn parse_response(
        resp: reqwest::Response,
        host: &str,
        port: u16,
        expected_id: u64,
    ) -> Result<Value, TestError> {
        let status = resp.status();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let text = resp.text().await?;

        if !status.is_success() {
            // A JSON-RPC error body on a non-2xx status is still the most precise report.
            if let Ok(parsed) = serde_json::from_str::<Value>(&text)
                && parsed.get("error").is_some()
            {
                return Ok(parsed);
            }
            let hint = if status == reqwest::StatusCode::UNAUTHORIZED {
                " — auth token missing or wrong (auth is on by default; the token is in \
                 <temp>/victauri/<pid>/token, or set VICTAURI_AUTH_TOKEN)"
            } else {
                ""
            };
            return Err(TestError::Connection {
                host: host.to_string(),
                port,
                reason: format!(
                    "tool call returned HTTP {status}{hint}; body: {}",
                    truncate_chars(&text, 200)
                ),
            });
        }

        if content_type.contains("text/event-stream") {
            find_sse_response(&text, expected_id).ok_or_else(|| TestError::Connection {
                host: host.to_string(),
                port,
                reason: format!(
                    "SSE stream contained no JSON-RPC response for request id {expected_id}; \
                     body: {}",
                    truncate_chars(&text, 200)
                ),
            })
        } else {
            serde_json::from_str(&text).map_err(|e| TestError::Connection {
                host: host.to_string(),
                port,
                reason: format!(
                    "JSON parse error: {e}, body: {}",
                    truncate_chars(&text, 200)
                ),
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

    /// Get a DOM snapshot of a specific webview by label.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn dom_snapshot_for(&mut self, label: &str) -> Result<Value, TestError> {
        self.call_tool("dom_snapshot", json!({"webview_label": label}))
            .await
    }

    /// Capture a screenshot of a specific webview by label.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn screenshot_for(&mut self, label: &str) -> Result<Value, TestError> {
        self.call_tool("screenshot", json!({"window_label": label}))
            .await
    }

    /// List running CSS animations/transitions (timing, easing, keyframes,
    /// target). Pass `selector` to scope, or `None` for all running animations.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn animation_list(&mut self, selector: Option<&str>) -> Result<Value, TestError> {
        let mut args = json!({ "action": "list" });
        if let Some(s) = selector {
            args["selector"] = json!(s);
        }
        self.call_tool("animation", args).await
    }

    /// Deterministically scrub the target's animation to `points` evenly-spaced
    /// steps, returning the geometry curve (and a filmstrip when `capture`).
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn animation_scrub(
        &mut self,
        selector: Option<&str>,
        points: usize,
        capture: bool,
    ) -> Result<Value, TestError> {
        let mut args = json!({ "action": "scrub", "points": points, "capture": capture });
        if let Some(s) = selector {
            args["selector"] = json!(s);
        }
        self.call_tool("animation", args).await
    }

    /// Arm the real-time motion recorder. Trigger the animation, then call
    /// [`VictauriClient::animation_sample_read`] to read the measured curve.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn animation_sample_arm(
        &mut self,
        selector: Option<&str>,
    ) -> Result<Value, TestError> {
        let mut args = json!({ "action": "sample", "record": true });
        if let Some(s) = selector {
            args["selector"] = json!(s);
        }
        self.call_tool("animation", args).await
    }

    /// Read back recorded motion sessions (per-frame curve + jank stats). Pass
    /// `clear` to reset sessions afterwards.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn animation_sample_read(&mut self, clear: bool) -> Result<Value, TestError> {
        self.call_tool(
            "animation",
            json!({ "action": "sample", "record": false, "clear": clear }),
        )
        .await
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
    /// Uses the server's default command timeout; for a slow command use
    /// [`Self::invoke_command_with_timeout`].
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn invoke_command(
        &mut self,
        command: &str,
        args: Option<Value>,
    ) -> Result<Value, TestError> {
        self.call_tool("invoke_command", invoke_command_params(command, args, None))
            .await
    }

    /// [`Self::invoke_command`] with an explicit server-side timeout (`timeout_ms`, capped
    /// by the server at 300 000 ms) — the option the server's own timeout error tells you to
    /// set for a slow command. The HTTP request timeout is extended to outlast it.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn invoke_command_with_timeout(
        &mut self,
        command: &str,
        args: Option<Value>,
        timeout_ms: u64,
    ) -> Result<Value, TestError> {
        self.call_tool(
            "invoke_command",
            invoke_command_params(command, args, Some(timeout_ms)),
        )
        .await
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

    /// Detect ghost commands, scoped to commands invoked within the last `since_ms`
    /// milliseconds.
    ///
    /// This is the non-destructive per-test pattern: invoke the suspect action, then
    /// call this with a small window (e.g. `5000`) so the `frontend_only` result
    /// reflects only the current test's traffic — not stale probe history accumulated
    /// in the session's IPC ring buffer. The alternative (`logs {action:'clear'}`)
    /// wipes the log for every reader.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn detect_ghost_commands_since(&mut self, since_ms: i64) -> Result<Value, TestError> {
        self.call_tool("detect_ghost_commands", json!({ "since_ms": since_ms }))
            .await
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

    /// Run environment diagnostics to detect potential compatibility issues.
    ///
    /// Checks for service workers, closed shadow DOM, iframes, large DOM,
    /// and CSP status. Returns warnings and environment info.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn get_diagnostics(&mut self) -> Result<Value, TestError> {
        self.call_tool("get_diagnostics", json!({})).await
    }

    // ── Backend Access ─────────────────────────────────────────────────────

    /// Get app info: Tauri config, directory paths, env vars, discovered databases.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn app_info(&mut self) -> Result<Value, TestError> {
        self.call_tool("app_info", json!({})).await
    }

    /// List files in an app directory (data, config, log, or `local_data`).
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn list_app_dir(
        &mut self,
        directory: Option<&str>,
        path: Option<&str>,
    ) -> Result<Value, TestError> {
        let mut args = json!({});
        if let Some(d) = directory {
            args["directory"] = json!(d);
        }
        if let Some(p) = path {
            args["path"] = json!(p);
        }
        self.call_tool("list_app_dir", args).await
    }

    /// Read a file from an app directory.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn read_app_file(
        &mut self,
        path: &str,
        directory: Option<&str>,
    ) -> Result<Value, TestError> {
        let mut args = json!({"path": path});
        if let Some(d) = directory {
            args["directory"] = json!(d);
        }
        self.call_tool("read_app_file", args).await
    }

    /// Execute a read-only SQL query against a `SQLite` database in the app data directory.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn query_db(
        &mut self,
        query: &str,
        db_path: Option<&str>,
        params: Option<Vec<Value>>,
    ) -> Result<Value, TestError> {
        let mut args = json!({"query": query});
        if let Some(p) = db_path {
            args["path"] = json!(p);
        }
        if let Some(params) = params {
            args["params"] = json!(params);
        }
        self.call_tool("query_db", args).await
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

    /// Poll a JavaScript expression until it is truthy (or equals `expected`),
    /// awaited server-side. This is the level-triggered, race-free way to wait
    /// for a fire-and-forget backend command to finish: pass an expression that
    /// reads a pollable status (it may `await`).
    ///
    /// Returns `{ ok: true, value, elapsed_ms }` on success or
    /// `{ ok: false, error, last_value, last_error, elapsed_ms }` on timeout.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn wait_for_expression(
        &mut self,
        expression: &str,
        expected: Option<Value>,
        timeout_ms: Option<u64>,
        poll_ms: Option<u64>,
    ) -> Result<Value, TestError> {
        let mut args = json!({ "condition": "expression", "value": expression });
        if let Some(e) = expected {
            args["expected"] = e;
        }
        if let Some(t) = timeout_ms {
            args["timeout_ms"] = json!(t);
        }
        if let Some(p) = poll_ms {
            args["poll_ms"] = json!(p);
        }
        self.call_tool("wait_for", args).await
    }

    /// Block until a named Tauri event fires on the app's event bus.
    ///
    /// Edge-triggered completion with a `since_ms` look-back (default 2000) so an
    /// event that fired between an `invoke_command` and this call is still caught.
    /// The app must emit the event and Victauri must capture it (custom events
    /// require `VictauriBuilder::listen_events`).
    ///
    /// Returns `{ ok: true, event: { name, payload, timestamp }, elapsed_ms }` on
    /// success or `{ ok: false, error, hint, elapsed_ms }` on timeout.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn wait_for_event(
        &mut self,
        event: &str,
        since_ms: Option<u64>,
        timeout_ms: Option<u64>,
        poll_ms: Option<u64>,
    ) -> Result<Value, TestError> {
        let mut args = json!({ "condition": "event", "value": event });
        if let Some(s) = since_ms {
            args["since_ms"] = json!(s);
        }
        if let Some(t) = timeout_ms {
            args["timeout_ms"] = json!(t);
        }
        if let Some(p) = poll_ms {
            args["poll_ms"] = json!(p);
        }
        self.call_tool("wait_for", args).await
    }

    /// Read application-defined backend state via a registered probe.
    ///
    /// With `probe = None`, returns `{ "probes": [names...] }`. With a probe name,
    /// returns that probe's JSON snapshot. Probes are registered by the app via
    /// `VictauriBuilder::probe`.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn app_state(&mut self, probe: Option<&str>) -> Result<Value, TestError> {
        let args = match probe {
            Some(name) => json!({ "probe": name }),
            None => json!({}),
        };
        self.call_tool("app_state", args).await
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

    /// Double-click an element by ref handle ID.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn double_click(&mut self, ref_id: &str) -> Result<Value, TestError> {
        self.call_tool(
            "interact",
            json!({"action": "double_click", "ref_id": ref_id}),
        )
        .await
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

    /// Get the host the client is connected to.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Get the port the client is connected to.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Get the MCP session ID. In stateless mode (the default) this is the constant compat id
    /// `"stateless"` the server backfills; empty string only if a server emits no header at all.
    #[must_use]
    pub fn session_id(&self) -> &str {
        self.session_id.as_deref().unwrap_or("")
    }

    pub(crate) fn http_client(&self) -> &reqwest::Client {
        &self.http
    }

    // ── IPC Log Helpers ───────────────────────────────────────────────────────

    /// Get IPC calls filtered to a specific command.
    ///
    /// Returns a Vec of all IPC log entries matching the given command name.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    #[deprecated(since = "0.2.0", note = "renamed to get_ipc_calls_for")]
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

    /// Get IPC calls made since a previous checkpoint.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    #[deprecated(since = "0.2.0", note = "renamed to get_ipc_calls_since")]
    pub async fn ipc_calls_since(&mut self, checkpoint: usize) -> Result<Vec<Value>, TestError> {
        // Timestamp filter, NOT positional skip: the server's log tools return a
        // capped sliding window of the NEWEST entries (default 100), so
        // `skip(checkpoint_length)` silently yields nothing the moment a busy app
        // exceeds the cap (found live on 4DA, 2026-08-11). Entries without a
        // timestamp are included only for the zero checkpoint (nothing to order by).
        let entries = self.ipc_log_entries().await?;
        Ok(entries
            .into_iter()
            .filter(|e| {
                e.get("timestamp")
                    .and_then(Value::as_u64)
                    .map_or(checkpoint == 0, |t| t > checkpoint as u64)
            })
            .collect())
    }

    /// Fetch the IPC log with the checkpoint read limit and normalize its shape.
    async fn ipc_log_entries(&mut self) -> Result<Vec<Value>, TestError> {
        // Explicit high limit: the tool's default window (100) is too small for a
        // busy app's checkpoint arithmetic; 1000 matches the JS-side log capacity.
        let log = self.get_ipc_log(Some(1000)).await?;
        if let Some(arr) = log.as_array() {
            Ok(arr.clone())
        } else if let Some(entries) = log.get("entries").and_then(Value::as_array) {
            Ok(entries.clone())
        } else {
            Ok(Vec::new())
        }
    }

    /// Filter the IPC log for calls to a specific command.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn get_ipc_calls_for(&mut self, command: &str) -> Result<Vec<Value>, TestError> {
        #[allow(deprecated)]
        self.get_ipc_calls(command).await
    }

    /// Get IPC calls made since a previous checkpoint.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn get_ipc_calls_since(
        &mut self,
        checkpoint: usize,
    ) -> Result<Vec<Value>, TestError> {
        #[allow(deprecated)]
        self.ipc_calls_since(checkpoint).await
    }

    // ── Builder-Style Wait (Phase 4B) ──────────────────────────────────────────

    /// Start a builder-style wait for a condition.
    ///
    /// This is a fluent alternative to [`VictauriClient::wait_for`] that avoids
    /// positional `Option` arguments.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// client.wait("text")
    ///     .value("Welcome")
    ///     .timeout_ms(5000)
    ///     .run()
    ///     .await
    ///     .unwrap();
    /// ```
    pub fn wait(&mut self, condition: &str) -> WaitForBuilder<'_> {
        WaitForBuilder {
            client: self,
            condition: condition.to_string(),
            value: None,
            timeout_ms: 10_000,
            poll_ms: 200,
        }
    }

    // ── Deprecated Aliases (Phase 4C) ────────────────────────────────────────

    /// Snapshot the newest IPC-log timestamp, for use with `ipc_calls_since`.
    ///
    /// Prefer [`VictauriClient::create_ipc_checkpoint`] — this alias exists
    /// for backwards compatibility.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    #[deprecated(since = "0.2.0", note = "renamed to create_ipc_checkpoint")]
    pub async fn ipc_checkpoint(&mut self) -> Result<usize, TestError> {
        self.create_ipc_checkpoint().await
    }

    /// Snapshot the newest IPC-log timestamp, for use with `ipc_calls_since`.
    ///
    /// Returns the maximum entry `timestamp` (epoch milliseconds) currently
    /// visible in the IPC log, or `0` when the log is empty. Pass this value to
    /// [`VictauriClient::ipc_calls_since`] to get only the calls that occurred
    /// after the checkpoint. For non-empty logs this method waits until the
    /// local clock has advanced past the checkpoint millisecond before it
    /// returns, so calls made immediately after the checkpoint cannot share the
    /// boundary timestamp and be filtered out by the strict `>` comparison.
    /// Timestamp-based rather than length-based because the server's log tools
    /// return a capped sliding window of the newest entries — a positional
    /// checkpoint stops working once a busy app exceeds the cap.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::call_tool`].
    pub async fn create_ipc_checkpoint(&mut self) -> Result<usize, TestError> {
        // The checkpoint is the NEWEST entry timestamp (epoch ms), not the log
        // length: the log tools serve a capped sliding window, so a length
        // snapshot breaks (always-empty `calls_since`) once the app has logged
        // more calls than the cap. Timestamps are window-position-independent.
        let entries = self.ipc_log_entries().await?;
        let checkpoint_ms = entries
            .iter()
            .filter_map(|e| e.get("timestamp").and_then(Value::as_u64))
            .max()
            .unwrap_or(0);
        wait_past_ipc_checkpoint_ms(checkpoint_ms).await;
        Ok(checkpoint_ms as usize)
    }

    // ── Typed Response Methods (Phase 4E) ────────────────────────────────────

    /// Read plugin info as a typed [`PluginInfo`] struct.
    ///
    /// This is a typed alternative to [`VictauriClient::get_plugin_info`] which
    /// returns raw JSON.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Other`] if the response cannot be deserialized.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn plugin_info(&mut self) -> Result<PluginInfo, TestError> {
        let value = self.get_plugin_info().await?;
        serde_json::from_value(value)
            .map_err(|e| TestError::Other(format!("failed to deserialize PluginInfo: {e}")))
    }

    /// Read process memory statistics as a typed [`MemoryStats`] struct.
    ///
    /// This is a typed alternative to [`VictauriClient::get_memory_stats`] which
    /// returns raw JSON.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Other`] if the response cannot be deserialized.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn memory_stats(&mut self) -> Result<MemoryStats, TestError> {
        let value = self.get_memory_stats().await?;
        serde_json::from_value(value)
            .map_err(|e| TestError::Other(format!("failed to deserialize MemoryStats: {e}")))
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

    /// Double-click the first element whose accessible text contains the given string.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no matching element is found.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn double_click_by_text(&mut self, text: &str) -> Result<Value, TestError> {
        let ref_id = self.find_ref_by_text(text).await?;
        self.double_click(&ref_id).await
    }

    /// Double-click the element with the given HTML `id` attribute.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element has the given id.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn double_click_by_id(&mut self, id: &str) -> Result<Value, TestError> {
        let ref_id = self.find_ref_by_id(id).await?;
        self.double_click(&ref_id).await
    }

    /// Double-click the first element matching a CSS selector.
    ///
    /// Resolves the selector via `find_elements`, then double-clicks the first match.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches the selector.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn double_click_by_selector(&mut self, selector: &str) -> Result<Value, TestError> {
        let ref_id = self.find_ref_by_selector(selector).await?;
        self.double_click(&ref_id).await
    }

    /// Click the first element matching a CSS selector.
    ///
    /// Resolves the selector via `find_elements`, then clicks the first match.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches the selector.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn click_by_selector(&mut self, selector: &str) -> Result<Value, TestError> {
        let ref_id = self.find_ref_by_selector(selector).await?;
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

    /// Fill an input whose accessible text contains the given string.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no matching element is found.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn fill_by_text(&mut self, text: &str, value: &str) -> Result<Value, TestError> {
        let ref_id = self.find_ref_by_text(text).await?;
        self.fill(&ref_id, value).await
    }

    /// Fill an input matching a CSS selector with the given value.
    ///
    /// Resolves the selector via `find_elements`, then fills the first match.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches the selector.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn fill_by_selector(
        &mut self,
        selector: &str,
        value: &str,
    ) -> Result<Value, TestError> {
        let ref_id = self.find_ref_by_selector(selector).await?;
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

    /// Select option(s) in a `<select>` element identified by HTML `id`.
    ///
    /// Accepts multiple values for multi-select elements.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element has the given id.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn select_option_by_id(
        &mut self,
        id: &str,
        values: &[&str],
    ) -> Result<Value, TestError> {
        let ref_id = self.find_ref_by_id(id).await?;
        self.select_option(&ref_id, values).await
    }

    /// Select option(s) in a `<select>` element whose accessible text contains
    /// the given string.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no matching element is found.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn select_option_by_text(
        &mut self,
        text: &str,
        values: &[&str],
    ) -> Result<Value, TestError> {
        let ref_id = self.find_ref_by_text(text).await?;
        self.select_option(&ref_id, values).await
    }

    /// Select option(s) in a `<select>` element matching a CSS selector.
    ///
    /// Resolves the selector via `find_elements`, then selects in the first match.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches the selector.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn select_option_by_selector(
        &mut self,
        selector: &str,
        values: &[&str],
    ) -> Result<Value, TestError> {
        let ref_id = self.find_ref_by_selector(selector).await?;
        self.select_option(&ref_id, values).await
    }

    /// Scroll an element matching a CSS selector into view.
    ///
    /// Resolves the selector via `find_elements`, then scrolls the first match.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element matches the selector.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn scroll_to_by_selector(&mut self, selector: &str) -> Result<Value, TestError> {
        let ref_id = self.find_ref_by_selector(selector).await?;
        self.scroll_to(&ref_id).await
    }

    /// Scroll an element with the given HTML `id` into view.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no element has the given id.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn scroll_to_by_id(&mut self, id: &str) -> Result<Value, TestError> {
        let ref_id = self.find_ref_by_id(id).await?;
        self.scroll_to(&ref_id).await
    }

    /// Scroll the first element whose accessible text contains the given string into view.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ElementNotFound`] if no matching element is found.
    /// Returns other errors from [`VictauriClient::call_tool`].
    pub async fn scroll_to_by_text(&mut self, text: &str) -> Result<Value, TestError> {
        let ref_id = self.find_ref_by_text(text).await?;
        self.scroll_to(&ref_id).await
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

    async fn find_ref_by_selector(&mut self, selector: &str) -> Result<String, TestError> {
        let result = self.find_elements(json!({"selector": selector})).await?;
        // find_elements returns an array of matched elements with ref_id fields
        let elements = result
            .as_array()
            .or_else(|| result.get("elements").and_then(Value::as_array));
        if let Some(elems) = elements
            && let Some(first) = elems.first()
            && let Some(ref_id) = first.get("ref_id").and_then(Value::as_str)
        {
            return Ok(ref_id.to_string());
        }
        Err(TestError::ElementNotFound(format!(
            "selector=\"{selector}\""
        )))
    }

    // ── Locator Factories ──────────────────────────────────────────────────

    /// Create a [`Locator`](crate::Locator) matching elements by ARIA role.
    ///
    /// Equivalent to Playwright's `page.getByRole()`.
    #[must_use]
    pub fn get_by_role(&self, role: &str) -> crate::locator::Locator {
        crate::locator::Locator::role(role)
    }

    /// Create a [`Locator`](crate::Locator) matching elements by visible text content.
    ///
    /// Equivalent to Playwright's `page.getByText()`.
    #[must_use]
    pub fn get_by_text(&self, text: &str) -> crate::locator::Locator {
        crate::locator::Locator::text(text)
    }

    /// Create a [`Locator`](crate::Locator) matching elements by `data-testid` attribute.
    ///
    /// Equivalent to Playwright's `page.getByTestId()`.
    #[must_use]
    pub fn get_by_test_id(&self, id: &str) -> crate::locator::Locator {
        crate::locator::Locator::test_id(id)
    }

    /// Create a [`Locator`](crate::Locator) matching form controls by associated label text.
    ///
    /// Equivalent to Playwright's `page.getByLabel()`.
    #[must_use]
    pub fn get_by_label(&self, text: &str) -> crate::locator::Locator {
        crate::locator::Locator::label(text)
    }

    /// Create a [`Locator`](crate::Locator) matching elements by placeholder text.
    ///
    /// Equivalent to Playwright's `page.getByPlaceholder()`.
    #[must_use]
    pub fn get_by_placeholder(&self, text: &str) -> crate::locator::Locator {
        crate::locator::Locator::placeholder(text)
    }

    /// Create a [`Locator`](crate::Locator) matching elements by CSS selector.
    ///
    /// Equivalent to Playwright's `page.locator()`.
    #[must_use]
    pub fn locator(&self, css: &str) -> crate::locator::Locator {
        crate::locator::Locator::css(css)
    }

    /// Create a [`Locator`](crate::Locator) matching elements by alt text (images).
    ///
    /// Equivalent to Playwright's `page.getByAltText()`.
    #[must_use]
    pub fn get_by_alt_text(&self, alt: &str) -> crate::locator::Locator {
        crate::locator::Locator::alt_text(alt)
    }

    /// Create a [`Locator`](crate::Locator) matching elements by title attribute.
    ///
    /// Equivalent to Playwright's `page.getByTitle()`.
    #[must_use]
    pub fn get_by_title(&self, title: &str) -> crate::locator::Locator {
        crate::locator::Locator::title(title)
    }

    // ── Screenshot to File ─────────────────────────────────────────────────

    /// Take a screenshot and save it to a file on disk.
    ///
    /// Captures the default window, decodes the base64 PNG, and writes it
    /// to the given path. Returns the canonical path of the saved file.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Other`] if the screenshot cannot be captured,
    /// decoded, or written to disk.
    pub async fn screenshot_to_file(
        &mut self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<std::path::PathBuf, TestError> {
        let result = self.screenshot().await?;
        let base64_data = extract_screenshot_base64(&result)?;
        save_screenshot_to_file(&base64_data, path.as_ref())
    }

    /// Take a screenshot of a specific window and save it to a file.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Other`] if the screenshot cannot be captured,
    /// decoded, or written to disk.
    pub async fn screenshot_to_file_for(
        &mut self,
        label: &str,
        path: impl AsRef<std::path::Path>,
    ) -> Result<std::path::PathBuf, TestError> {
        let result = self.screenshot_for(label).await?;
        let base64_data = extract_screenshot_base64(&result)?;
        save_screenshot_to_file(&base64_data, path.as_ref())
    }
}

/// `invoke_command` tool arguments.
fn invoke_command_params(command: &str, args: Option<Value>, timeout_ms: Option<u64>) -> Value {
    let mut params = json!({"command": command});
    if let Some(a) = args {
        params["args"] = a;
    }
    if let Some(ms) = timeout_ms {
        params["timeout_ms"] = json!(ms);
    }
    params
}

fn save_screenshot_to_file(
    base64_data: &str,
    path: &std::path::Path,
) -> Result<std::path::PathBuf, TestError> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_data)
        .map_err(|e| TestError::Other(format!("failed to decode screenshot base64: {e}")))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| TestError::Other(format!("failed to create directory: {e}")))?;
    }
    std::fs::write(path, &bytes)
        .map_err(|e| TestError::Other(format!("failed to write screenshot: {e}")))?;
    path.canonicalize()
        .or_else(|_| Ok(path.to_path_buf()))
        .map_err(|e: std::io::Error| TestError::Other(format!("path error: {e}")))
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
    if let Some(data) = result
        .pointer("/result/content/0/data")
        .and_then(Value::as_str)
    {
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

#[cfg(test)]
mod transport_tests {
    use super::*;

    #[test]
    fn truncate_chars_never_splits_a_code_point() {
        // "a" + 2-byte chars: byte 200 falls mid-"é", where `&odd[..200]` panics.
        let odd = format!("a{}", "é".repeat(300));
        assert!(!odd.is_char_boundary(200));
        assert_eq!(truncate_chars(&odd, 200).chars().count(), 200);
        assert_eq!(truncate_chars("short", 200), "short");
        assert_eq!(truncate_chars("日本語テキスト", 3), "日本語");
    }

    #[test]
    fn request_timeout_outlasts_tool_wait() {
        // No `timeout_ms`: the server bounds the call by the app's eval timeout (up to 300 s),
        // so the client must outlast that ceiling, not give up at 60 s and invite a retry.
        assert_eq!(request_timeout_for(&json!({})), Duration::from_secs(340));
        assert_eq!(
            request_timeout_for(&json!({"timeout_ms": 5_000})),
            DEFAULT_REQUEST_TIMEOUT
        );
        assert_eq!(
            request_timeout_for(&json!({"timeout_ms": 120_000})),
            Duration::from_secs(160)
        );
        // Clamped the way the server clamps it.
        assert_eq!(
            request_timeout_for(&json!({"timeout_ms": u64::MAX})),
            Duration::from_secs(340)
        );
    }

    #[test]
    fn invoke_command_with_timeout_sends_timeout_ms_and_outlasts_it() {
        // R4-TC1: the server tells a slow command's caller to set `timeout_ms`, but the
        // client had no way to send it.
        let params = invoke_command_params("slow", Some(json!({"n": 1})), Some(200_000));
        assert_eq!(params["command"], "slow");
        assert_eq!(params["args"]["n"], 1);
        assert_eq!(params["timeout_ms"], 200_000);
        assert!(request_timeout_for(&params) > Duration::from_millis(200_000));
        // The plain form is unchanged.
        assert_eq!(
            invoke_command_params("fast", None, None),
            json!({"command": "fast"})
        );
    }

    /// A minimal HTTP server answering every request with `status` and an empty body, which
    /// records the JSON bodies it received.
    async fn fixed_status_server(
        status: u16,
    ) -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let record = std::sync::Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let record = std::sync::Arc::clone(&record);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 16 * 1024];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    record
                        .lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&buf[..n]).into_owned());
                    let reply = format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = sock.write_all(reply.as_bytes()).await;
                });
            }
        });
        (port, seen)
    }

    #[test]
    fn a_rate_limited_health_answer_means_alive() {
        // R4-NET1.
        assert!(health_status_means_alive(200));
        assert!(health_status_means_alive(204));
        assert!(health_status_means_alive(429));
        assert!(!health_status_means_alive(401));
        assert!(!health_status_means_alive(404));
        assert!(!health_status_means_alive(500));
        assert!(!health_status_means_alive(503));
    }

    #[tokio::test]
    async fn is_alive_treats_a_rate_limited_server_as_alive() {
        // R4-NET1: an unauthenticated /health flood (→ 429) must not make a live app look dead.
        let (port, _) = fixed_status_server(429).await;
        assert!(client_at(port, false, None).is_alive().await);
        let (port, _) = fixed_status_server(503).await;
        assert!(!client_at(port, false, None).is_alive().await);
    }

    #[test]
    fn sse_response_is_matched_by_id_not_position() {
        let body = "id: 0\ndata:\n\n\
                    data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{}}\n\n\
                    data: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"stale\":true}}\n\n\
                    data: {\"jsonrpc\":\"2.0\",\"id\":42,\"result\":{\"ok\":true}}\n\n";
        let msg = find_sse_response(body, 42).expect("response for id 42");
        assert_eq!(msg["result"]["ok"], json!(true));
        assert!(find_sse_response(body, 99).is_none());
    }

    /// A port that refuses connections for as long as the returned socket lives: BOUND but
    /// never listening. (Binding then releasing — the old approach — let another process on a
    /// busy CI runner grab the port, turning "connection refused" into a non-HTTP reply.)
    fn closed_port() -> (u16, tokio::net::TcpSocket) {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let port = socket.local_addr().unwrap().port();
        (port, socket)
    }

    fn client_at(port: u16, discovered: bool, app_identifier: Option<&str>) -> VictauriClient {
        VictauriClient {
            http: reqwest::Client::new(),
            base_url: format!("http://127.0.0.1:{port}"),
            host: "127.0.0.1".to_string(),
            port,
            session_id: None,
            next_id: 10,
            auth_token: None,
            discovered,
            app_identifier: app_identifier.map(str::to_string),
        }
    }

    fn explicit_discovery_env() -> bool {
        std::env::var_os("VICTAURI_PORT").is_some()
            || std::env::var_os("VICTAURI_AUTH_TOKEN").is_some()
    }

    #[tokio::test]
    async fn connection_refused_on_discovered_client_rediscovers_same_app_only() {
        if explicit_discovery_env() {
            return; // explicit endpoint config bypasses discovery by design
        }
        let (port, _held) = closed_port();
        // An identity no live app can have: rediscovery must find nothing and say so,
        // instead of binding to whatever app holds the default port.
        let id = format!("com.victauri.test.absent-{}", std::process::id());
        let mut client = client_at(port, true, Some(&id));
        let err = client
            .call_tool("get_plugin_info", json!({}))
            .await
            .expect_err("nothing is listening");
        match err {
            TestError::Connection { reason, .. } => {
                assert!(
                    reason.contains(&id),
                    "a connection error on a discovered client must trigger an identity-pinned \
                     rediscovery: {reason}"
                );
            }
            other => panic!("expected a rediscovery Connection error, got {other:?}"),
        }
        assert_eq!(client.port(), port, "no default-port (7373) fallback");
        assert_eq!(client.app_identifier(), Some(id.as_str()));
    }

    #[tokio::test]
    async fn connection_refused_on_explicit_client_is_not_rediscovered() {
        let (port, _held) = closed_port();
        let mut client = client_at(port, false, None);
        let err = client
            .call_tool("get_plugin_info", json!({}))
            .await
            .expect_err("nothing is listening");
        assert!(
            matches!(err, TestError::Request(ref e) if e.is_connect()),
            "an explicit-endpoint client surfaces the transport error as-is: {err:?}"
        );
        assert_eq!(client.port(), port);
    }

    #[test]
    fn rediscover_without_a_match_leaves_the_endpoint_untouched() {
        if explicit_discovery_env() {
            return;
        }
        let (port, _held) = closed_port();
        let id = format!("com.victauri.test.absent2-{}", std::process::id());
        let mut client = client_at(port, true, Some(&id));
        client.auth_token = Some("keep".to_string());
        assert!(client.rediscover().is_err());
        assert_eq!(client.port, port);
        assert_eq!(client.base_url, format!("http://127.0.0.1:{port}"));
        assert_eq!(client.auth_token.as_deref(), Some("keep"));
    }

    #[test]
    fn sse_multi_line_data_is_joined() {
        let body = "event: message\r\ndata: {\"jsonrpc\":\"2.0\",\r\ndata: \"id\":3,\"error\":{\"code\":-1}}\r\n\r\n";
        let msg = find_sse_response(body, 3).expect("joined multi-line event");
        assert_eq!(msg["error"]["code"], json!(-1));
    }
}
