//! Stdio-to-HTTP bridge for MCP clients like Claude Code.
//!
//! Reads JSON-RPC messages from stdin, forwards them to Victauri's Streamable HTTP
//! endpoint, parses SSE responses, and writes them back to stdout. This bridges
//! the gap between MCP hosts that expect stdio transport and Victauri's HTTP server.
//!
//! The bridge automatically recovers from server restarts: when it detects a stale
//! session (404) or connection failure, it re-discovers the server and retries.

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};

const MAX_RETRIES: usize = 3;
const RETRY_DELAY_MS: u64 = 500;

/// Run the stdio bridge against a discovered Victauri server.
///
/// # Errors
///
/// Returns an error if the server cannot be reached or the bridge encounters
/// a fatal protocol error.
pub async fn run(wait: bool) -> Result<()> {
    let connection = Arc::new(Mutex::new(discover_server(wait).await?));
    let session_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .context("failed to create HTTP client")?;

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let msg: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("victauri-bridge: invalid JSON on stdin: {e}");
                continue;
            }
        };

        let is_notification = msg.get("id").is_none();

        let mut last_err = None;

        for attempt in 0..MAX_RETRIES {
            let (port, token) = {
                let guard = connection.lock().expect("connection lock");
                (guard.0, guard.1.clone())
            };
            let mcp_url = format!("http://127.0.0.1:{port}/mcp");

            let mut req = http
                .post(&mcp_url)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream");

            if let Some(ref t) = token {
                req = req.header("Authorization", format!("Bearer {t}"));
            }
            {
                let sid_guard = session_id.lock().expect("session_id lock");
                if let Some(ref sid) = *sid_guard {
                    req = req.header("Mcp-Session-Id", sid.clone());
                }
            }

            let resp = match req.json(&msg).send().await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "victauri-bridge: connection failed (attempt {}/{}): {e}",
                        attempt + 1,
                        MAX_RETRIES
                    );
                    *session_id.lock().expect("session_id lock") = None;
                    if attempt + 1 < MAX_RETRIES {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            RETRY_DELAY_MS * (attempt as u64 + 1),
                        ))
                        .await;
                        if let Ok(new_conn) = discover_server(true).await {
                            *connection.lock().expect("connection lock") = new_conn;
                            eprintln!("victauri-bridge: reconnected to server");
                        }
                    }
                    last_err = Some(format!("Victauri server unreachable: {e}"));
                    continue;
                }
            };

            if let Some(sid) = resp.headers().get("mcp-session-id")
                && let Ok(s) = sid.to_str()
            {
                *session_id.lock().expect("session_id lock") = Some(s.to_string());
            }

            let status = resp.status();

            if is_notification && status.as_u16() == 202 {
                last_err = None;
                break;
            }

            if status.as_u16() == 404 || status.as_u16() == 409 {
                eprintln!(
                    "victauri-bridge: stale session ({}), reconnecting (attempt {}/{})",
                    status,
                    attempt + 1,
                    MAX_RETRIES
                );
                *session_id.lock().expect("session_id lock") = None;
                if attempt + 1 < MAX_RETRIES {
                    tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS)).await;
                    if let Ok(new_conn) = discover_server(false).await {
                        *connection.lock().expect("connection lock") = new_conn;
                    }
                }
                last_err = Some(format!("Victauri returned {status}"));
                continue;
            }

            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                if !is_notification {
                    let err_resp = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": msg.get("id"),
                        "error": {
                            "code": -32000,
                            "message": format!("Victauri returned {status}: {body}")
                        }
                    });
                    let mut out = stdout.lock();
                    let _ = writeln!(out, "{err_resp}");
                    let _ = out.flush();
                }
                last_err = None;
                break;
            }

            let content_type = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();

            let body = resp.text().await.unwrap_or_default();

            if content_type.contains("text/event-stream") {
                for sse_line in body.lines() {
                    if let Some(data) = sse_line.strip_prefix("data: ") {
                        let data = data.trim();
                        if data.is_empty() {
                            continue;
                        }
                        if serde_json::from_str::<serde_json::Value>(data).is_ok() {
                            let mut out = stdout.lock();
                            let _ = writeln!(out, "{data}");
                            let _ = out.flush();
                        }
                    }
                }
            } else {
                let body = body.trim();
                if !body.is_empty() {
                    let mut out = stdout.lock();
                    let _ = writeln!(out, "{body}");
                    let _ = out.flush();
                }
            }

            last_err = None;
            break;
        }

        if let Some(err_msg) = last_err
            && !is_notification
        {
            let err_resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": msg.get("id"),
                "error": {
                    "code": -32000,
                    "message": err_msg
                }
            });
            let mut out = stdout.lock();
            let _ = writeln!(out, "{err_resp}");
            let _ = out.flush();
        }
    }

    Ok(())
}

/// Discover a running Victauri server's port and auth token.
///
/// # Errors
///
/// Returns an error if no running server can be found within the timeout.
async fn discover_server(wait: bool) -> Result<(u16, Option<String>)> {
    let max_attempts = if wait { 30 } else { 3 };
    let delay = std::time::Duration::from_secs(1);

    for attempt in 0..max_attempts {
        let port = discover_port();
        let token = discover_token();

        let url = format!("http://127.0.0.1:{port}/health");
        let ok = reqwest::Client::new()
            .get(&url)
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success());

        if ok {
            eprintln!("victauri-bridge: connected to Victauri on port {port}");
            return Ok((port, token));
        }

        if attempt < max_attempts - 1 {
            if attempt == 0 {
                eprintln!("victauri-bridge: waiting for Victauri server...");
            }
            tokio::time::sleep(delay).await;
        }
    }

    bail!(
        "Could not connect to Victauri server.\n\
         Is your Tauri app running? Start it with: pnpm run tauri dev"
    )
}

/// Scan discovery directories for a running server's port.
fn discover_port() -> u16 {
    if let Ok(p) = std::env::var("VICTAURI_PORT")
        && let Ok(port) = p.parse::<u16>()
    {
        return port;
    }
    // Scan temp/victauri/<PID>/port files for live servers
    let discovery_root = std::env::temp_dir().join("victauri");
    if let Ok(entries) = std::fs::read_dir(&discovery_root) {
        for entry in entries.filter_map(Result::ok) {
            let port_file = entry.path().join("port");
            if let Ok(content) = std::fs::read_to_string(&port_file)
                && let Ok(port) = content.trim().parse::<u16>()
            {
                let pid_str = entry.file_name().to_string_lossy().to_string();
                if let Ok(pid) = pid_str.parse::<u32>()
                    && is_process_alive(pid)
                {
                    return port;
                }
            }
        }
    }
    7373
}

/// Scan discovery directories for a running server's auth token.
fn discover_token() -> Option<String> {
    if let Ok(token) = std::env::var("VICTAURI_AUTH_TOKEN") {
        return Some(token);
    }
    let discovery_root = std::env::temp_dir().join("victauri");
    if let Ok(entries) = std::fs::read_dir(&discovery_root) {
        for entry in entries.filter_map(Result::ok) {
            let token_file = entry.path().join("token");
            if let Ok(content) = std::fs::read_to_string(&token_file) {
                let token = content.trim().to_string();
                if !token.is_empty() {
                    let pid_str = entry.file_name().to_string_lossy().to_string();
                    if let Ok(pid) = pid_str.parse::<u32>()
                        && is_process_alive(pid)
                    {
                        return Some(token);
                    }
                }
            }
        }
    }
    None
}

#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    use std::process::Command;
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .is_ok_and(|o| {
            let out = String::from_utf8_lossy(&o.stdout);
            out.contains(&pid.to_string())
        })
}

#[cfg(not(windows))]
fn is_process_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}
