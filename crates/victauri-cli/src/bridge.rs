//! Stdio-to-HTTP bridge for MCP clients like Claude Code.
//!
//! Reads JSON-RPC messages from stdin, forwards them to Victauri's Streamable HTTP
//! endpoint, parses SSE responses, and writes them back to stdout. This bridges
//! the gap between MCP hosts that expect stdio transport and Victauri's HTTP server.
//!
//! Why this exists (and why agents should connect through it, not a fixed `url:`):
//!
//! * **Always reaches the RIGHT app.** A static `.mcp.json` URL hardcodes a port; when
//!   several Victauri apps run (or one falls back off a busy 7373), that port can point at
//!   the WRONG process. The bridge resolves the live backend **by app identity** at connect
//!   time and re-resolves on failure — so the agent can never get stuck talking to the
//!   wrong app. Select with `--app <identifier>` (or `VICTAURI_APP`); with no selector it
//!   uses the single running app, or errors clearly if several are running.
//! * **Survives server restarts.** Every dev rebuild/relaunch invalidates the MCP session.
//!   The bridge caches the `initialize` handshake and transparently re-establishes a fresh
//!   session (re-discovering the port) on a stale session (404/409/422) or connection drop,
//!   so the agent's tool calls keep working without a reconnect.

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};

const MAX_RETRIES: usize = 4;
const RETRY_DELAY_MS: u64 = 400;

/// A discovered, live Victauri backend.
#[derive(Clone, Debug)]
struct ServerInfo {
    port: u16,
    token: Option<String>,
    identifier: Option<String>,
    product_name: Option<String>,
}

impl ServerInfo {
    fn label(&self) -> String {
        let name = self
            .identifier
            .as_deref()
            .or(self.product_name.as_deref())
            .unwrap_or("<unknown app>");
        format!("{name} (port {})", self.port)
    }
}

/// Run the stdio bridge against a discovered Victauri server.
///
/// `app` selects which app to bind when several are running (matches the Tauri bundle
/// identifier or product name; falls back to the `VICTAURI_APP` env var).
///
/// # Errors
///
/// Returns an error if no matching server can be reached.
pub async fn run(wait: bool, app: Option<String>) -> Result<()> {
    let app = app.or_else(|| std::env::var("VICTAURI_APP").ok());
    let connection = Arc::new(Mutex::new(discover_and_select(wait, app.as_deref()).await?));
    let session_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    // Cache the MCP handshake so a session can be re-established transparently after the
    // backend restarts — the host (e.g. Claude Code) only sends `initialize` once.
    let cached_init: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
    // Set once an `initialize` succeeds with no `Mcp-Session-Id`: the server is running in
    // stateless mode, so there is no session to mint or lose. In that mode we must NOT
    // re-`initialize` before every request (it would double every call for no benefit) and
    // there is no stale-session class to recover from.
    let stateless: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));

    let http = build_client()?;

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

        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let is_initialize = method == "initialize";
        if is_initialize {
            *cached_init.lock().expect("cached_init lock") = Some(msg.clone());
        }
        let is_notification = msg.get("id").is_none();

        let mut last_err = None;

        for attempt in 0..MAX_RETRIES {
            // If the session was invalidated and we have a cached handshake, re-establish a
            // fresh session BEFORE replaying the real request (skip when the message itself
            // is the initialize). This is what makes restart-recovery actually work — the
            // old code cleared the session then re-sent the tool call with no session, which
            // the server rejects with 422 "expect initialize".
            if !is_initialize && !*stateless.lock().expect("stateless lock") {
                let need_reinit = session_id.lock().expect("session lock").is_none();
                if need_reinit {
                    let init = cached_init.lock().expect("cached_init lock").clone();
                    if let Some(init) = init {
                        let (port, token) = conn_parts(&connection);
                        // We don't relay the re-init response to the host; it already believes
                        // it is initialized. On failure we fall through to the request attempt,
                        // which triggers re-discovery below.
                        if let Ok(out) =
                            post_message(&http, port, token.as_deref(), None, &init).await
                            && let Some(sid) = out.session_id
                        {
                            *session_id.lock().expect("session lock") = Some(sid);
                        }
                    }
                }
            }

            let (port, token) = conn_parts(&connection);
            let sid = session_id.lock().expect("session lock").clone();

            match post_message(&http, port, token.as_deref(), sid.as_deref(), &msg).await {
                Ok(out) => {
                    if let Some(new_sid) = out.session_id {
                        *session_id.lock().expect("session lock") = Some(new_sid);
                    } else if is_initialize && !out.stale_session {
                        // initialize succeeded but returned no session id → stateless server.
                        *stateless.lock().expect("stateless lock") = true;
                    }

                    if out.stale_session {
                        eprintln!(
                            "victauri-bridge: stale session (HTTP {}), re-establishing (attempt {}/{})",
                            out.status,
                            attempt + 1,
                            MAX_RETRIES
                        );
                        *session_id.lock().expect("session lock") = None;
                        if attempt + 1 < MAX_RETRIES {
                            tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS))
                                .await;
                            if let Ok(new_conn) = discover_and_select(false, app.as_deref()).await {
                                *connection.lock().expect("conn lock") = new_conn;
                            }
                        }
                        last_err = Some(format!("Victauri returned {}", out.status));
                        continue;
                    }

                    if is_notification && out.accepted {
                        last_err = None;
                        break;
                    }

                    for payload in out.payloads {
                        let mut o = stdout.lock();
                        let _ = writeln!(o, "{payload}");
                        let _ = o.flush();
                    }
                    last_err = None;
                    break;
                }
                Err(e) => {
                    eprintln!(
                        "victauri-bridge: connection failed (attempt {}/{}): {e}",
                        attempt + 1,
                        MAX_RETRIES
                    );
                    *session_id.lock().expect("session lock") = None;
                    if attempt + 1 < MAX_RETRIES {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            RETRY_DELAY_MS * (attempt as u64 + 1),
                        ))
                        .await;
                        if let Ok(new_conn) = discover_and_select(true, app.as_deref()).await {
                            *connection.lock().expect("conn lock") = new_conn;
                            eprintln!("victauri-bridge: reconnected to {}", {
                                let g = connection.lock().expect("conn lock");
                                g.label()
                            });
                        }
                    }
                    last_err = Some(format!("Victauri server unreachable: {e}"));
                    continue;
                }
            }
        }

        if let Some(err_msg) = last_err
            && !is_notification
        {
            let err_resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": msg.get("id"),
                "error": { "code": -32000, "message": err_msg }
            });
            let mut o = stdout.lock();
            let _ = writeln!(o, "{err_resp}");
            let _ = o.flush();
        }
    }

    Ok(())
}

fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(Into::into)
}

fn conn_parts(connection: &Arc<Mutex<ServerInfo>>) -> (u16, Option<String>) {
    let g = connection.lock().expect("conn lock");
    (g.port, g.token.clone())
}

/// Outcome of forwarding one JSON-RPC message to the backend.
struct PostOutcome {
    status: u16,
    session_id: Option<String>,
    stale_session: bool,
    accepted: bool,
    payloads: Vec<String>,
}

/// Forward a single JSON-RPC message to `127.0.0.1:<port>/mcp` and parse the response.
async fn post_message(
    http: &reqwest::Client,
    port: u16,
    token: Option<&str>,
    session_id: Option<&str>,
    msg: &serde_json::Value,
) -> Result<PostOutcome> {
    let url = format!("http://127.0.0.1:{port}/mcp");
    let mut req = http
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    if let Some(sid) = session_id {
        req = req.header("Mcp-Session-Id", sid);
    }

    let resp = req.json(msg).send().await?;
    let status = resp.status().as_u16();
    let new_sid = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    // 404/409 = unknown/terminated session; 422 = "expect initialize" (no/!init session).
    // All three mean "the session is gone — re-establish it".
    let stale_session = matches!(status, 404 | 409 | 422);
    let accepted = status == 202;

    let mut payloads = Vec::new();
    if !stale_session && status != 202 {
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = resp.text().await.unwrap_or_default();

        if !(200..300).contains(&status) {
            // Surface a JSON-RPC error for the original request id.
            payloads.push(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": msg.get("id"),
                    "error": { "code": -32000, "message": format!("Victauri returned {status}: {body}") }
                })
                .to_string(),
            );
        } else if content_type.contains("text/event-stream") {
            for sse_line in body.lines() {
                if let Some(data) = sse_line.strip_prefix("data: ") {
                    let data = data.trim();
                    if !data.is_empty() && serde_json::from_str::<serde_json::Value>(data).is_ok() {
                        payloads.push(data.to_string());
                    }
                }
            }
        } else {
            let body = body.trim();
            if !body.is_empty() {
                payloads.push(body.to_string());
            }
        }
    }

    Ok(PostOutcome {
        status,
        session_id: new_sid,
        stale_session,
        accepted,
        payloads,
    })
}

/// Discover live Victauri backends and select the one matching `app` (or the only one).
async fn discover_and_select(wait: bool, app: Option<&str>) -> Result<ServerInfo> {
    let max_attempts = if wait { 30 } else { 3 };
    let delay = std::time::Duration::from_secs(1);

    for attempt in 0..max_attempts {
        // Explicit env override wins (a developer pinning a specific port).
        if let Ok(p) = std::env::var("VICTAURI_PORT")
            && let Ok(port) = p.parse::<u16>()
            && health_ok(port).await
        {
            return Ok(ServerInfo {
                port,
                token: std::env::var("VICTAURI_AUTH_TOKEN")
                    .ok()
                    .or_else(|| discover_token_for_port(port)),
                identifier: None,
                product_name: None,
            });
        }

        let mut servers = discover_servers();
        // Keep only live ones (health-checked).
        let mut live = Vec::new();
        for s in servers.drain(..) {
            if health_ok(s.port).await {
                live.push(s);
            }
        }

        match select(&live, app) {
            Selection::One(s) => {
                eprintln!("victauri-bridge: connected to {}", s.label());
                return Ok(s);
            }
            Selection::None if attempt + 1 < max_attempts => {
                if attempt == 0 {
                    eprintln!("victauri-bridge: waiting for Victauri server...");
                }
                tokio::time::sleep(delay).await;
            }
            Selection::None => {
                bail!(
                    "Could not connect to Victauri server.\n\
                     Is your Tauri app running (debug build)? Start it with: pnpm run tauri dev"
                );
            }
            Selection::Ambiguous(labels) => {
                bail!(
                    "Multiple Victauri apps are running:\n  {}\n\
                     Specify which one with `victauri bridge --app <identifier>` (or set \
                     VICTAURI_APP). The identifier is your Tauri bundle identifier.",
                    labels.join("\n  ")
                );
            }
        }
    }

    bail!("Could not connect to a matching Victauri server")
}

enum Selection {
    One(ServerInfo),
    None,
    Ambiguous(Vec<String>),
}

/// Pick the server matching `app`, or the sole running server.
fn select(live: &[ServerInfo], app: Option<&str>) -> Selection {
    if live.is_empty() {
        return Selection::None;
    }
    if let Some(app) = app {
        let needle = app.to_ascii_lowercase();
        // Prefer an exact identifier/product_name match, then a substring match.
        let exact = live.iter().find(|s| {
            s.identifier
                .as_deref()
                .map(str::to_ascii_lowercase)
                .as_deref()
                == Some(&needle)
                || s.product_name
                    .as_deref()
                    .map(str::to_ascii_lowercase)
                    .as_deref()
                    == Some(&needle)
        });
        if let Some(s) = exact {
            return Selection::One(s.clone());
        }
        let partial = live.iter().find(|s| {
            s.identifier
                .as_deref()
                .is_some_and(|i| i.to_ascii_lowercase().contains(&needle))
                || s.product_name
                    .as_deref()
                    .is_some_and(|p| p.to_ascii_lowercase().contains(&needle))
        });
        return match partial {
            Some(s) => Selection::One(s.clone()),
            None => Selection::None,
        };
    }
    // No app specified: fine if exactly one is running; ambiguous otherwise.
    if live.len() == 1 {
        Selection::One(live[0].clone())
    } else {
        Selection::Ambiguous(live.iter().map(ServerInfo::label).collect())
    }
}

/// Scan `<temp>/victauri/<pid>/` for live-process discovery entries (port + token + identity).
fn discover_servers() -> Vec<ServerInfo> {
    let root = std::env::temp_dir().join("victauri");
    let mut out = Vec::new();
    // The root itself is security-sensitive: its owner can rename a trusted PID
    // directory after our child check and swap in attacker-controlled files.
    if !dir_is_trusted(&root) {
        return out;
    }
    let Ok(entries) = std::fs::read_dir(&root) else {
        return out;
    };
    for entry in entries.filter_map(Result::ok) {
        let pid_str = entry.file_name().to_string_lossy().to_string();
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if !is_process_alive(pid) {
            continue;
        }
        let dir = entry.path();
        // Shared-temp hardening (audit #15, read side). The discovery root lives under a
        // world-writable temp dir on Unix, so a local attacker can plant a fake `<pid>`
        // directory — named after one of THEIR own live processes, so `is_process_alive`
        // passes — pointing at a server they control, and harvest the real Bearer token we
        // send it (and feed us forged tool results). Trust a directory only if it is a real
        // directory we own and is not group/other-writable — the same guard
        // `victauri-test::discovery` already applies. The bridge is the path Claude Code
        // connects through, so this is the highest-value read-side sink.
        if !dir_is_trusted(&dir) {
            continue;
        }
        let Ok(port_s) = std::fs::read_to_string(dir.join("port")) else {
            continue;
        };
        let Ok(port) = port_s.trim().parse::<u16>() else {
            continue;
        };
        let token = std::fs::read_to_string(dir.join("token"))
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        let (identifier, product_name) = std::fs::read_to_string(dir.join("metadata.json"))
            .ok()
            .and_then(|m| serde_json::from_str::<serde_json::Value>(&m).ok())
            .map_or((None, None), |m| {
                (
                    m.get("identifier")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    m.get("product_name")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                )
            });
        out.push(ServerInfo {
            port,
            token,
            identifier,
            product_name,
        });
    }
    out
}

/// Token belonging to the exact server selected by a `VICTAURI_PORT` override.
///
/// Never send a token discovered for one app to an unrelated localhost port.
fn discover_token_for_port(port: u16) -> Option<String> {
    token_for_port(&discover_servers(), port)
}

fn token_for_port(servers: &[ServerInfo], port: u16) -> Option<String> {
    servers
        .iter()
        .find(|server| server.port == port)
        .and_then(|server| server.token.clone())
}

async fn health_ok(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/health");
    reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
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
    // Portable POSIX liveness check. `/proc` is Linux-only — on macOS it does not exist,
    // so the old `/proc/{pid}` test always returned false and the bridge filtered out every
    // discovery entry (it could find NO server on macOS). `kill -0` sends no signal but
    // succeeds iff the process exists and is signalable by us — and discovery entries are
    // our own user's processes. Works identically on macOS and Linux.
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Trust a discovery directory only if it is a real directory (not a symlink), owned by the
/// current user, and not group/other-writable. Mirrors `victauri-test::discovery::dir_is_trusted`
/// — the bridge had no such check (audit #15 read-side residual), and it is the path Claude Code
/// connects through. No `unsafe` (this crate is `#![forbid(unsafe_code)]`): the effective uid is
/// read back from an exclusively-created probe file.
#[cfg(unix)]
fn dir_is_trusted(path: &std::path::Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.file_type().is_dir() {
        return false; // reject symlinks / non-dirs
    }
    let Some(euid) = current_euid() else {
        return false; // can't establish our uid -> don't trust
    };
    meta.uid() == euid && (meta.permissions().mode() & 0o022) == 0
}

#[cfg(unix)]
fn current_euid() -> Option<u32> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_PROBE: AtomicU64 = AtomicU64::new(0);
    for _ in 0..16 {
        let sequence = NEXT_PROBE.fetch_add(1, Ordering::Relaxed);
        let probe = std::env::temp_dir().join(format!(
            ".victauri_bridge_uidprobe_{}_{}",
            std::process::id(),
            sequence
        ));
        if let Some(uid) = uid_from_exclusive_probe(&probe) {
            return Some(uid);
        }
    }
    None
}

/// Create a UID probe without following a pre-planted symlink in the shared temp dir.
#[cfg(unix)]
fn uid_from_exclusive_probe(probe: &std::path::Path) -> Option<u32> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(probe)
        .ok()?;
    let uid = file.metadata().ok().map(|m| m.uid());
    drop(file);
    let _ = std::fs::remove_file(probe);
    uid
}

/// On Windows the per-user temp dir is not world-writable, so the shared-temp planting
/// attack does not apply; trust the directory.
#[cfg(not(unix))]
fn dir_is_trusted(_path: &std::path::Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn uid_probe_refuses_preplanted_symlink_without_clobbering_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let probe = dir.path().join("probe");
        std::fs::write(&target, "must-survive").unwrap();
        std::os::unix::fs::symlink(&target, &probe).unwrap();

        assert_eq!(uid_from_exclusive_probe(&probe), None);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "must-survive");
    }

    fn srv(id: &str, name: &str, port: u16) -> ServerInfo {
        ServerInfo {
            port,
            token: None,
            identifier: Some(id.to_string()),
            product_name: Some(name.to_string()),
        }
    }

    #[test]
    fn selects_sole_server_without_app() {
        let live = vec![srv("com.a.app", "A", 7373)];
        assert!(matches!(select(&live, None), Selection::One(s) if s.port == 7373));
    }

    #[test]
    fn ambiguous_when_multiple_and_no_app() {
        let live = vec![srv("com.a.app", "A", 7373), srv("com.b.app", "B", 7374)];
        assert!(matches!(select(&live, None), Selection::Ambiguous(v) if v.len() == 2));
    }

    #[test]
    fn selects_by_identifier_among_many() {
        let live = vec![srv("com.a.app", "A", 7373), srv("com.4da.app", "4DA", 7374)];
        match select(&live, Some("com.4da.app")) {
            Selection::One(s) => assert_eq!(s.port, 7374),
            _ => panic!("should pick 4DA by identifier"),
        }
    }

    #[test]
    fn selects_by_product_name_case_insensitive() {
        let live = vec![
            srv("com.a.app", "Demo", 7373),
            srv("com.4da.app", "4DA", 7374),
        ];
        match select(&live, Some("4da")) {
            Selection::One(s) => assert_eq!(s.port, 7374),
            _ => panic!("should pick by product name"),
        }
    }

    #[test]
    fn no_match_returns_none() {
        let live = vec![srv("com.a.app", "A", 7373)];
        assert!(matches!(
            select(&live, Some("com.nope.app")),
            Selection::None
        ));
    }

    #[test]
    fn token_selection_never_crosses_ports() {
        let mut first = srv("com.a.app", "A", 7373);
        first.token = Some("token-a".to_string());
        let mut second = srv("com.b.app", "B", 7374);
        second.token = Some("token-b".to_string());
        let servers = vec![first, second];

        assert_eq!(token_for_port(&servers, 7374).as_deref(), Some("token-b"));
        assert_eq!(token_for_port(&servers, 7999), None);
    }

    #[test]
    fn substring_identifier_match() {
        let live = vec![srv("com.victauri.demo", "Demo", 7373)];
        match select(&live, Some("demo")) {
            Selection::One(s) => assert_eq!(s.port, 7373),
            _ => panic!("substring of product/identifier should match"),
        }
    }

    // End-to-end against REAL discovery files: the plugin writes port/token/metadata.json
    // under `<temp>/victauri/<pid>/`; this proves the bridge parses those real files and can
    // select the right app by identity — even amid the many stale dirs left by dead processes.
    #[test]
    fn discover_servers_reads_real_metadata_and_selects() {
        let pid = std::process::id(); // alive → passes is_process_alive
        let dir = std::env::temp_dir().join("victauri").join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        // Make ownership/permissions deterministic so `dir_is_trusted` passes regardless of
        // the runner's umask (a umask of 002 would otherwise leave the dir group-writable
        // and the read-side trust guard would correctly reject it).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(dir.join("port"), "61999").unwrap();
        std::fs::write(dir.join("token"), "tok-xyz").unwrap();
        std::fs::write(
            dir.join("metadata.json"),
            r#"{"pid":1,"port":61999,"identifier":"com.test.discover","product_name":"DiscoverTest"}"#,
        )
        .unwrap();

        let servers = discover_servers();
        let mine = servers
            .iter()
            .find(|s| s.identifier.as_deref() == Some("com.test.discover"))
            .expect("bridge should discover the entry written for the live current pid");
        assert_eq!(mine.port, 61999);
        assert_eq!(mine.token.as_deref(), Some("tok-xyz"));
        assert_eq!(mine.product_name.as_deref(), Some("DiscoverTest"));

        // And selection by identity picks it out.
        assert!(matches!(
            select(std::slice::from_ref(mine), Some("com.test.discover")),
            Selection::One(_)
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Audit #15 read-side: a planted, world-writable discovery dir (the shape an attacker
    // creates in a shared /tmp) must NOT be trusted, so its token is never read/sent.
    #[cfg(unix)]
    #[test]
    fn dir_is_trusted_rejects_world_writable_and_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("vic_trust_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        // Owned, 0700 -> trusted.
        let good = base.join("good");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::set_permissions(&good, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(dir_is_trusted(&good), "0700 owner dir must be trusted");

        // Group/other-writable -> rejected.
        let bad = base.join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!dir_is_trusted(&bad), "world-writable dir must be rejected");

        // Symlink (even to a trusted target) -> rejected (no symlink following).
        let link = base.join("link");
        let _ = std::os::unix::fs::symlink(&good, &link);
        assert!(!dir_is_trusted(&link), "symlinked dir must be rejected");

        let _ = std::fs::remove_dir_all(&base);
    }
}
