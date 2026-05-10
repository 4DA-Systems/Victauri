use std::io::BufRead;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::VictauriClient;
use crate::error::TestError;

/// Maximum number of stderr lines retained in the ring buffer.
const STDERR_MAX_LINES: usize = 50;

/// Number of stderr lines included in error messages.
const STDERR_DISPLAY_LINES: usize = 10;

/// Managed Tauri application lifecycle for integration testing.
///
/// Spawns a Tauri app as a child process, waits for the Victauri MCP server
/// to become healthy, and provides connected [`VictauriClient`] instances.
/// The app is killed when the `TestApp` is dropped.
///
/// Stderr output from the spawned process is captured in a background thread
/// and the last few lines are included in error messages when the app fails
/// to start or times out, making startup failures much easier to diagnose.
///
/// # Example
///
/// ```rust,ignore
/// use victauri_test::TestApp;
///
/// #[tokio::test]
/// async fn my_app_works() {
///     let app = TestApp::spawn("cargo run -p my-app").await.unwrap();
///     let mut client = app.client().await.unwrap();
///     client.click_by_text("Submit").await.unwrap();
///     client.expect_text("Success").await.unwrap();
/// }
/// ```
pub struct TestApp {
    child: Option<Child>,
    port: u16,
    token: Option<String>,
    stderr_lines: Arc<Mutex<Vec<String>>>,
    _stderr_thread: Option<std::thread::JoinHandle<()>>,
}

impl TestApp {
    /// Spawn an application from a shell command and wait for it to become ready.
    ///
    /// Polls the Victauri health endpoint until it responds (up to 30 seconds).
    /// Uses port auto-discovery via temp files, falling back to env vars and defaults.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Connection`] if the app fails to start or the health
    /// endpoint doesn't respond within the timeout.
    pub async fn spawn(cmd: &str) -> Result<Self, TestError> {
        Self::spawn_with_options(cmd, None, Duration::from_secs(30)).await
    }

    /// Spawn with explicit port and timeout configuration.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Connection`] if the app fails to start or the health
    /// endpoint doesn't respond within the timeout.
    pub async fn spawn_with_options(
        cmd: &str,
        port: Option<u16>,
        timeout: Duration,
    ) -> Result<Self, TestError> {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if parts.is_empty() {
            return Err(TestError::Connection {
                host: "127.0.0.1".into(),
                port: port.unwrap_or(0),
                reason: "empty command".into(),
            });
        }

        let mut child = Command::new(parts[0])
            .args(&parts[1..])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| TestError::Connection {
                host: "127.0.0.1".into(),
                port: port.unwrap_or(0),
                reason: format!("failed to spawn `{cmd}`: {e}"),
            })?;

        let (stderr_lines, stderr_thread) = spawn_stderr_reader(child.stderr.take());

        let mut app = Self {
            child: Some(child),
            port: port.unwrap_or(0),
            token: None,
            stderr_lines,
            _stderr_thread: stderr_thread,
        };

        app.wait_for_ready(timeout).await?;
        Ok(app)
    }

    /// Spawn the bundled demo app from the workspace.
    ///
    /// Equivalent to `TestApp::spawn("cargo run -p demo-app")` but with
    /// appropriate environment variables set.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Connection`] if the demo app fails to start.
    pub async fn spawn_demo() -> Result<Self, TestError> {
        let port = discover_port();
        let parts = ["cargo", "run", "-p", "demo-app"];

        let mut child = Command::new(parts[0])
            .args(&parts[1..])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| TestError::Connection {
                host: "127.0.0.1".into(),
                port,
                reason: format!("failed to spawn demo-app: {e}"),
            })?;

        let (stderr_lines, stderr_thread) = spawn_stderr_reader(child.stderr.take());

        let mut app = Self {
            child: Some(child),
            port,
            token: None,
            stderr_lines,
            _stderr_thread: stderr_thread,
        };

        app.wait_for_ready(Duration::from_secs(60)).await?;
        Ok(app)
    }

    /// Connect to an already-running Victauri app (no process management).
    ///
    /// Useful when the app is started externally (e.g., by CI or a dev script).
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Connection`] if the health endpoint doesn't respond.
    pub async fn attach(port: u16, token: Option<String>) -> Result<Self, TestError> {
        let app = Self {
            child: None,
            port,
            token,
            stderr_lines: Arc::new(Mutex::new(Vec::new())),
            _stderr_thread: None,
        };

        let http = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{port}/health");
        let resp = http
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| TestError::Connection {
                host: "127.0.0.1".into(),
                port,
                reason: format!("health check failed: {e}"),
            })?;

        if !resp.status().is_success() {
            return Err(TestError::Connection {
                host: "127.0.0.1".into(),
                port,
                reason: format!("health returned {}", resp.status()),
            });
        }

        Ok(app)
    }

    /// Create a new connected [`VictauriClient`] for this app.
    ///
    /// Each call returns a fresh MCP session.
    ///
    /// # Errors
    ///
    /// Returns errors from [`VictauriClient::connect_with_token`].
    pub async fn client(&self) -> Result<VictauriClient, TestError> {
        VictauriClient::connect_with_token(self.port, self.token.as_deref()).await
    }

    /// The port the MCP server is running on.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    async fn wait_for_ready(&mut self, timeout: Duration) -> Result<(), TestError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| TestError::Connection {
                host: "127.0.0.1".into(),
                port: self.port,
                reason: e.to_string(),
            })?;

        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(200);

        loop {
            if start.elapsed() > timeout {
                let stderr_tail = self.recent_stderr();
                return Err(TestError::Connection {
                    host: "127.0.0.1".into(),
                    port: self.port,
                    reason: format!(
                        "app did not become ready within {}s — check that the Victauri plugin is \
                         initialized and the MCP server is listening.{stderr_tail}",
                        timeout.as_secs()
                    ),
                });
            }

            if let Some(ref mut child) = self.child
                && let Some(status) = child.try_wait().ok().flatten()
            {
                let stderr_tail = self.recent_stderr();
                return Err(TestError::Connection {
                    host: "127.0.0.1".into(),
                    port: self.port,
                    reason: format!(
                        "app process exited with {status} before becoming ready{stderr_tail}"
                    ),
                });
            }

            let port = self.discover_actual_port();
            let url = format!("http://127.0.0.1:{port}/health");

            if let Ok(resp) = http.get(&url).send().await
                && resp.status().is_success()
            {
                self.port = port;
                self.token = discover_token();
                return Ok(());
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Format the last N captured stderr lines for inclusion in error messages.
    fn recent_stderr(&self) -> String {
        let lines = self
            .stderr_lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if lines.is_empty() {
            return String::new();
        }
        let start = lines.len().saturating_sub(STDERR_DISPLAY_LINES);
        let tail: Vec<&str> = lines[start..].iter().map(String::as_str).collect();
        format!(
            "\n\nApp stderr (last {} lines):\n  {}",
            tail.len(),
            tail.join("\n  ")
        )
    }

    fn discover_actual_port(&self) -> u16 {
        if self.port != 0 {
            return self.port;
        }
        discover_port()
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Spawn a background thread that drains the child's stderr into a bounded ring buffer.
///
/// Returns the shared line buffer and an optional join handle. The thread exits
/// naturally when the child's stderr pipe is closed (i.e., the process exits).
fn spawn_stderr_reader(
    stderr: Option<std::process::ChildStderr>,
) -> (Arc<Mutex<Vec<String>>>, Option<std::thread::JoinHandle<()>>) {
    let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let handle = stderr.map(|pipe| {
        let lines = Arc::clone(&lines);
        std::thread::Builder::new()
            .name("victauri-stderr-reader".into())
            .spawn(move || {
                let reader = std::io::BufReader::new(pipe);
                for line in reader.lines() {
                    match line {
                        Ok(text) => {
                            let mut buf = lines
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            if buf.len() >= STDERR_MAX_LINES {
                                buf.remove(0);
                            }
                            buf.push(text);
                        }
                        Err(_) => break,
                    }
                }
            })
            .expect("failed to spawn stderr reader thread")
    });

    (lines, handle)
}

fn discover_port() -> u16 {
    if let Ok(p) = std::env::var("VICTAURI_PORT")
        && let Ok(port) = p.parse::<u16>()
    {
        return port;
    }
    if let Some(port) = crate::discovery::scan_discovery_dirs_for_port() {
        return port;
    }
    // Legacy fallback
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
    if let Some(token) = crate::discovery::scan_discovery_dirs_for_token() {
        return Some(token);
    }
    // Legacy fallback
    let path = std::env::temp_dir().join("victauri.token");
    let token = std::fs::read_to_string(&path).ok()?;
    let token = token.trim().to_string();
    if token.is_empty() { None } else { Some(token) }
}
