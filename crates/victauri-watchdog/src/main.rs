//! Watchdog process that monitors and restarts the Victauri MCP server if it becomes unresponsive.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Minimum poll interval (seconds). A zero/sub-second interval would let a
/// permanently-failing health check spin into a busy loop, so we floor it.
const MIN_INTERVAL_SECS: u64 = 1;
/// Default poll interval (seconds) when unset or unparseable.
const DEFAULT_INTERVAL_SECS: u64 = 5;
/// Minimum consecutive failures before a recovery action fires. A value of 0
/// would mean "recover on the very first (or every) poll", so we floor it at 1.
const MIN_MAX_FAILURES: u32 = 1;
/// Default consecutive-failure threshold when unset or unparseable.
const DEFAULT_MAX_FAILURES: u32 = 3;
/// Hard timeout for a recovery command. A hung recovery (e.g. a command that
/// blocks forever) must not wedge the watchdog itself, so the child is killed
/// after this many seconds and the failure is reported.
const RECOVERY_TIMEOUT_SECS: u64 = 60;

/// Port polled when none is configured and discovery finds no single live app.
const DEFAULT_PORT: u16 = 7373;

struct Config {
    /// Port to poll: the explicit one (`VICTAURI_PORT` / positional `PORT`) or
    /// [`DEFAULT_PORT`]. When not explicit, `main` resolves it via discovery.
    port: u16,
    /// `true` when the port was set explicitly — discovery is then skipped
    /// entirely, preserving the pre-discovery behaviour exactly.
    port_explicit: bool,
    /// App selector for discovery (`--app <id>` / `VICTAURI_APP`): matches the
    /// Tauri bundle identifier or product name in the discovery `metadata.json`.
    app: Option<String>,
    interval: Duration,
    max_failures: u32,
    on_failure_cmd: Option<String>,
}

impl Config {
    fn from_env() -> Self {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let (arg_port, arg_app) = parse_args(&args);
        let explicit_port = std::env::var("VICTAURI_PORT")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .or(arg_port);
        Self {
            port: explicit_port.unwrap_or(DEFAULT_PORT),
            port_explicit: explicit_port.is_some(),
            app: arg_app.or_else(|| {
                std::env::var("VICTAURI_APP")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            }),
            interval: clamp_interval(std::env::var("VICTAURI_INTERVAL").ok().as_deref()),
            max_failures: clamp_max_failures(
                std::env::var("VICTAURI_MAX_FAILURES").ok().as_deref(),
            ),
            on_failure_cmd: std::env::var("VICTAURI_ON_FAILURE").ok(),
        }
    }
}

/// Parse CLI args: an optional positional `PORT` and `--app <id>` / `--app=<id>`.
fn parse_args(args: &[String]) -> (Option<u16>, Option<String>) {
    let mut port = None;
    let mut app = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--app" {
            app = iter.next().cloned();
        } else if let Some(v) = arg.strip_prefix("--app=") {
            app = Some(v.to_string());
        } else if port.is_none()
            && let Ok(p) = arg.parse::<u16>()
        {
            port = Some(p);
        }
    }
    (port, app.filter(|a| !a.is_empty()))
}

// ── Discovery ────────────────────────────────────────────────────────────────
//
// The plugin writes `<temp>/victauri/<pid>/port` (+ `metadata.json` with the app
// `identifier` / `product_name`) and may land on 7374+ when 7373 is taken, so a
// fixed port can watch the wrong app or nothing at all. `/health` needs no auth,
// so only the port is read — never the token.

/// Outcome of scanning the discovery directory.
#[derive(Debug, PartialEq, Eq)]
enum Discovery {
    /// Exactly one live app matched.
    Found(u16),
    /// No live app matched.
    None,
    /// Several live apps matched and no `--app` selector disambiguates them.
    Ambiguous(Vec<(u32, u16)>),
}

fn discovery_base_dir() -> PathBuf {
    std::env::temp_dir().join("victauri")
}

/// Scan `base` for live Victauri apps (owning pid alive), optionally filtered by
/// `app` against the discovery `metadata.json` identity.
fn discover_in(base: &Path, app: Option<&str>, is_alive: impl Fn(u32) -> bool) -> Discovery {
    let Ok(entries) = std::fs::read_dir(base) else {
        return Discovery::None;
    };
    let mut found: Vec<(u32, u16)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(pid) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if !dir_is_trusted(&path) || !is_alive(pid) {
            continue;
        }
        let Some(port) = std::fs::read_to_string(path.join("port"))
            .ok()
            .and_then(|s| s.trim().parse::<u16>().ok())
            .filter(|p| *p != 0)
        else {
            continue;
        };
        if let Some(want) = app {
            let meta: serde_json::Value = std::fs::read_to_string(path.join("metadata.json"))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            let matches = ["identifier", "product_name"]
                .iter()
                .any(|k| meta.get(k).and_then(|v| v.as_str()) == Some(want));
            if !matches {
                continue;
            }
        }
        found.push((pid, port));
    }
    match found.as_slice() {
        [] => Discovery::None,
        [(_, port)] => Discovery::Found(*port),
        _ => {
            found.sort_unstable();
            Discovery::Ambiguous(found)
        }
    }
}

/// On Unix the temp root is world-writable: only trust a real (non-symlink)
/// directory that is not group/other-writable. Windows temp is per-user.
#[cfg(unix)]
fn dir_is_trusted(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(path)
        .is_ok_and(|m| m.file_type().is_dir() && (m.permissions().mode() & 0o022) == 0)
}

#[cfg(not(unix))]
fn dir_is_trusted(path: &Path) -> bool {
    path.is_dir()
}

/// Cross-platform "is this pid a live process?" (same approach as victauri-test).
#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
}

#[cfg(not(windows))]
fn is_process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Resolve the port to poll from discovery, logging the outcome. Returns `None`
/// when discovery yields no single app (the caller keeps its current port).
fn discover_port(app: Option<&str>) -> Option<u16> {
    match discover_in(&discovery_base_dir(), app, is_process_alive) {
        Discovery::Found(port) => Some(port),
        Discovery::None => None,
        Discovery::Ambiguous(apps) => {
            tracing::warn!(
                ?apps,
                "Several Victauri apps are running — set --app <identifier> (or VICTAURI_APP) \
                 or VICTAURI_PORT to choose one"
            );
            None
        }
    }
}

/// Parse `VICTAURI_INTERVAL` (seconds) and clamp it up to `MIN_INTERVAL_SECS`.
///
/// Unset or unparseable falls back to `DEFAULT_INTERVAL_SECS`. A configured
/// value below the floor (including 0) is clamped UP to the floor and emits a
/// warning — a zero interval must never cause a busy loop.
fn clamp_interval(raw: Option<&str>) -> Duration {
    let secs = match raw {
        Some(s) => match s.trim().parse::<u64>() {
            Ok(v) => v,
            Err(_) => DEFAULT_INTERVAL_SECS,
        },
        None => DEFAULT_INTERVAL_SECS,
    };
    if secs < MIN_INTERVAL_SECS {
        tracing::warn!(
            configured = secs,
            floor = MIN_INTERVAL_SECS,
            "VICTAURI_INTERVAL below minimum — clamping up to floor to avoid a busy loop"
        );
        Duration::from_secs(MIN_INTERVAL_SECS)
    } else {
        Duration::from_secs(secs)
    }
}

/// Parse `VICTAURI_MAX_FAILURES` and clamp it up to `MIN_MAX_FAILURES`.
///
/// Unset or unparseable falls back to `DEFAULT_MAX_FAILURES`. A configured
/// value below 1 (i.e. 0) would mean "recover immediately / every poll" and is
/// clamped UP to 1 with a warning.
fn clamp_max_failures(raw: Option<&str>) -> u32 {
    let value = match raw {
        Some(s) => match s.trim().parse::<u32>() {
            Ok(v) => v,
            Err(_) => DEFAULT_MAX_FAILURES,
        },
        None => DEFAULT_MAX_FAILURES,
    };
    if value < MIN_MAX_FAILURES {
        tracing::warn!(
            configured = value,
            floor = MIN_MAX_FAILURES,
            "VICTAURI_MAX_FAILURES below minimum — clamping up to floor to avoid immediate recovery"
        );
        MIN_MAX_FAILURES
    } else {
        value
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("victauri-watchdog {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("victauri-watchdog {}", env!("CARGO_PKG_VERSION"));
        println!("Crash-recovery sidecar for Victauri MCP server\n");
        println!("USAGE: victauri-watchdog [PORT] [--app <identifier>]\n");
        println!("Without an explicit port, the port is discovered from");
        println!("<temp>/victauri/<pid>/port (live apps only), falling back to 7373.\n");
        println!("OPTIONS:");
        println!("  --app <id>       Watch the app with this bundle identifier / product name");
        println!("  -h, --help       Print help");
        println!("  -V, --version    Print version\n");
        println!("ENVIRONMENT:");
        println!("  VICTAURI_PORT           Server port (skips discovery)");
        println!("  VICTAURI_APP            Same as --app");
        println!("  VICTAURI_INTERVAL       Poll interval in seconds (default: 5, min: 1)");
        println!(
            "  VICTAURI_MAX_FAILURES   Consecutive failures before action (default: 3, min: 1)"
        );
        println!("  VICTAURI_ON_FAILURE     Shell command to run on failure");
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env();
    let mut port = config.port;
    if !config.port_explicit {
        if let Some(p) = discover_port(config.app.as_deref()) {
            tracing::info!(port = p, app = ?config.app, "Discovered Victauri app");
            port = p;
        } else {
            tracing::info!(
                port,
                app = ?config.app,
                "No single live Victauri app discovered yet — polling the default port"
            );
        }
    }
    let mut url = format!("http://127.0.0.1:{port}/health");

    tracing::info!(
        port,
        interval_secs = config.interval.as_secs(),
        max_failures = config.max_failures,
        on_failure = config.on_failure_cmd.as_deref().unwrap_or("(none)"),
        "Victauri watchdog started"
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;

    let mut consecutive_failures: u32 = 0;
    let mut action_fired = false;

    loop {
        tokio::time::sleep(config.interval).await;

        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                if consecutive_failures > 0 {
                    tracing::info!(
                        after_failures = consecutive_failures,
                        "Victauri MCP server recovered"
                    );
                    consecutive_failures = 0;
                    action_fired = false;
                }
            }
            Ok(resp) => {
                consecutive_failures += 1;
                tracing::warn!(
                    status = %resp.status(),
                    failure_count = consecutive_failures,
                    "Health check returned non-success status"
                );
            }
            Err(e) => {
                consecutive_failures += 1;
                tracing::warn!(
                    error = %e,
                    failure_count = consecutive_failures,
                    "Health check failed"
                );
            }
        }

        // The app may have restarted on a different port (7373 taken → 7374+). With
        // no explicit port, re-resolve via discovery on failure and follow it; the
        // next successful poll then logs the recovery. Discovery only runs while
        // failing, so a healthy app costs no extra process spawns.
        if consecutive_failures > 0
            && !config.port_explicit
            && let Some(p) = discover_port(config.app.as_deref())
            && p != port
        {
            tracing::info!(
                from = port,
                to = p,
                "Victauri app moved — following discovered port"
            );
            port = p;
            url = format!("http://127.0.0.1:{port}/health");
        }

        if consecutive_failures >= config.max_failures && !action_fired {
            tracing::error!(
                failure_count = consecutive_failures,
                "Victauri MCP server unreachable — the Tauri app may have crashed"
            );

            if let Some(ref cmd) = config.on_failure_cmd {
                // Do NOT log the full command line at info level — it may carry
                // secrets, tokens, or sensitive paths. Surface only the program
                // (first whitespace-delimited token). The full string is kept at
                // debug level for operators who explicitly opt in.
                tracing::info!(
                    program = recovery_program_name(cmd),
                    "Executing recovery action"
                );
                tracing::debug!(command = cmd, "Full recovery command line");
                match run_recovery(cmd).await {
                    Ok(status) => {
                        tracing::info!(exit_code = ?status.code(), "Recovery action completed");
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Recovery action failed to execute");
                    }
                }
            }

            action_fired = true;
        }
    }
}

/// Extract the program name (first whitespace-delimited token) from a recovery
/// command line for non-sensitive logging. Returns `"(empty)"` for a
/// blank/whitespace-only command. This is a best-effort label for logs only —
/// the command is still executed verbatim through the shell.
fn recovery_program_name(cmd: &str) -> &str {
    cmd.split_whitespace().next().unwrap_or("(empty)")
}

async fn run_recovery(cmd: &str) -> anyhow::Result<std::process::ExitStatus> {
    // Spawn (not `.status()`) so we can enforce a timeout and kill a hung child —
    // a recovery command that never returns must not block the watchdog forever.
    let mut child = if cfg!(windows) {
        tokio::process::Command::new("cmd")
            .args(["/C", cmd])
            .spawn()?
    } else {
        tokio::process::Command::new("sh")
            .args(["-c", cmd])
            .spawn()?
    };
    match tokio::time::timeout(Duration::from_secs(RECOVERY_TIMEOUT_SECS), child.wait()).await {
        Ok(status) => Ok(status?),
        Err(_elapsed) => {
            // Kill the wrapping shell so the watchdog loop is freed. (A grandchild
            // the shell spawned may outlive it; the watchdog's job is to not wedge.)
            let _ = child.kill().await;
            anyhow::bail!(
                "recovery command timed out after {RECOVERY_TIMEOUT_SECS}s and was killed"
            );
        }
    }
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_env() {
        // SAFETY: test-only — ENV_LOCK serializes all env access in this module.
        unsafe {
            std::env::remove_var("VICTAURI_PORT");
            std::env::remove_var("VICTAURI_INTERVAL");
            std::env::remove_var("VICTAURI_MAX_FAILURES");
            std::env::remove_var("VICTAURI_ON_FAILURE");
            std::env::remove_var("VICTAURI_APP");
        }
    }

    #[test]
    fn parse_args_reads_port_and_app() {
        let a = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(parse_args(&a(&[])), (None, None));
        assert_eq!(parse_args(&a(&["7374"])), (Some(7374), None));
        assert_eq!(
            parse_args(&a(&["--app", "com.x.app", "7380"])),
            (Some(7380), Some("com.x.app".to_string()))
        );
        assert_eq!(
            parse_args(&a(&["--app=Demo"])),
            (None, Some("Demo".to_string()))
        );
    }

    fn write_entry(base: &Path, pid: u32, port: &str, identifier: Option<&str>) {
        let dir = base.join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(dir.join("port"), port).unwrap();
        if let Some(id) = identifier {
            let meta = serde_json::json!({"pid": pid, "identifier": id, "product_name": "P"});
            std::fs::write(dir.join("metadata.json"), meta.to_string()).unwrap();
        }
    }

    #[test]
    fn discovery_ignores_dead_pids_and_bad_ports() {
        let tmp = tempfile::tempdir().unwrap();
        write_entry(tmp.path(), 100, "7374", Some("com.a"));
        write_entry(tmp.path(), 200, "7373", Some("com.b")); // dead
        write_entry(tmp.path(), 300, "not-a-port", None);
        std::fs::create_dir_all(tmp.path().join("not-a-pid")).unwrap();
        let alive = |pid: u32| pid != 200;
        assert_eq!(discover_in(tmp.path(), None, alive), Discovery::Found(7374));
    }

    #[test]
    fn discovery_selects_by_app_identity() {
        let tmp = tempfile::tempdir().unwrap();
        write_entry(tmp.path(), 100, "7373", Some("com.a"));
        write_entry(tmp.path(), 200, "7375", Some("com.b"));
        let alive = |_| true;
        assert_eq!(
            discover_in(tmp.path(), None, alive),
            Discovery::Ambiguous(vec![(100, 7373), (200, 7375)])
        );
        assert_eq!(
            discover_in(tmp.path(), Some("com.b"), alive),
            Discovery::Found(7375)
        );
        // product_name also matches (both entries share "P" here → ambiguous).
        assert!(matches!(
            discover_in(tmp.path(), Some("P"), alive),
            Discovery::Ambiguous(_)
        ));
        assert_eq!(
            discover_in(tmp.path(), Some("com.missing"), alive),
            Discovery::None
        );
        assert_eq!(
            discover_in(&tmp.path().join("absent"), None, alive),
            Discovery::None
        );
    }

    #[test]
    fn config_defaults() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_env();
        let config = Config::from_env();
        assert_eq!(config.port, 7373);
        assert!(!config.port_explicit);
        assert_eq!(config.interval, Duration::from_secs(5));
        assert_eq!(config.max_failures, 3);
        assert!(config.on_failure_cmd.is_none());
    }

    #[test]
    fn config_from_env_vars() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_env();
        // SAFETY: test-only — ENV_LOCK serializes all env access in this module.
        unsafe {
            std::env::set_var("VICTAURI_PORT", "9999");
            std::env::set_var("VICTAURI_INTERVAL", "10");
            std::env::set_var("VICTAURI_MAX_FAILURES", "5");
            std::env::set_var("VICTAURI_ON_FAILURE", "echo recovered");
        }
        let config = Config::from_env();
        assert_eq!(config.port, 9999);
        assert!(config.port_explicit, "VICTAURI_PORT must bypass discovery");
        assert_eq!(config.interval, Duration::from_secs(10));
        assert_eq!(config.max_failures, 5);
        assert_eq!(config.on_failure_cmd, Some("echo recovered".to_string()));
        clear_env();
    }

    #[test]
    fn config_invalid_env_uses_defaults() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_env();
        // SAFETY: test-only — ENV_LOCK serializes all env access in this module.
        unsafe {
            std::env::set_var("VICTAURI_PORT", "not_a_number");
            std::env::set_var("VICTAURI_INTERVAL", "abc");
            std::env::set_var("VICTAURI_MAX_FAILURES", "xyz");
        }
        let config = Config::from_env();
        assert_eq!(config.port, 7373);
        assert_eq!(config.interval, Duration::from_secs(5));
        assert_eq!(config.max_failures, 3);
        clear_env();
    }

    #[test]
    fn config_zero_interval_is_clamped() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_env();
        // SAFETY: test-only — ENV_LOCK serializes all env access in this module.
        unsafe {
            std::env::set_var("VICTAURI_INTERVAL", "0");
            std::env::set_var("VICTAURI_MAX_FAILURES", "0");
        }
        let config = Config::from_env();
        assert_eq!(config.interval, Duration::from_secs(MIN_INTERVAL_SECS));
        assert_eq!(config.max_failures, MIN_MAX_FAILURES);
        clear_env();
    }

    #[test]
    fn clamp_interval_floors_zero_and_sub_floor() {
        // Zero must clamp UP to the floor (never a busy loop).
        assert_eq!(
            clamp_interval(Some("0")),
            Duration::from_secs(MIN_INTERVAL_SECS)
        );
    }

    #[test]
    fn clamp_interval_preserves_valid_values() {
        assert_eq!(clamp_interval(Some("10")), Duration::from_secs(10));
        // Exactly at the floor is preserved.
        assert_eq!(
            clamp_interval(Some("1")),
            Duration::from_secs(MIN_INTERVAL_SECS)
        );
    }

    #[test]
    fn clamp_interval_defaults_when_unset_or_garbage() {
        assert_eq!(
            clamp_interval(None),
            Duration::from_secs(DEFAULT_INTERVAL_SECS)
        );
        assert_eq!(
            clamp_interval(Some("not_a_number")),
            Duration::from_secs(DEFAULT_INTERVAL_SECS)
        );
        // Whitespace is trimmed before parsing.
        assert_eq!(clamp_interval(Some("  7  ")), Duration::from_secs(7));
    }

    #[test]
    fn clamp_max_failures_floors_zero() {
        // Zero would mean "recover every poll" — clamp UP to 1.
        assert_eq!(clamp_max_failures(Some("0")), MIN_MAX_FAILURES);
    }

    #[test]
    fn clamp_max_failures_preserves_valid_values() {
        assert_eq!(clamp_max_failures(Some("5")), 5);
        assert_eq!(clamp_max_failures(Some("1")), MIN_MAX_FAILURES);
    }

    #[test]
    fn clamp_max_failures_defaults_when_unset_or_garbage() {
        assert_eq!(clamp_max_failures(None), DEFAULT_MAX_FAILURES);
        assert_eq!(clamp_max_failures(Some("xyz")), DEFAULT_MAX_FAILURES);
        assert_eq!(clamp_max_failures(Some("  4  ")), 4);
    }

    #[test]
    fn recovery_program_name_extracts_first_token() {
        assert_eq!(
            recovery_program_name("restart-app --token=SECRET --path /home/u"),
            "restart-app"
        );
        assert_eq!(recovery_program_name("echo"), "echo");
        assert_eq!(recovery_program_name("   "), "(empty)");
        assert_eq!(recovery_program_name(""), "(empty)");
    }

    #[tokio::test]
    async fn recovery_runs_echo() {
        let cmd = "echo ok";
        let status = run_recovery(cmd).await.unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    async fn recovery_bad_command_fails() {
        let cmd = if cfg!(windows) { "exit /b 1" } else { "exit 1" };
        let status = run_recovery(cmd).await.unwrap();
        assert!(!status.success());
    }
}
