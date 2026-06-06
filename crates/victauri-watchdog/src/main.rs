//! Watchdog process that monitors and restarts the Victauri MCP server if it becomes unresponsive.

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

struct Config {
    port: u16,
    interval: Duration,
    max_failures: u32,
    on_failure_cmd: Option<String>,
}

impl Config {
    fn from_env() -> Self {
        Self {
            port: std::env::var("VICTAURI_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .or_else(|| std::env::args().nth(1).and_then(|s| s.parse().ok()))
                .unwrap_or(7373),
            interval: clamp_interval(std::env::var("VICTAURI_INTERVAL").ok().as_deref()),
            max_failures: clamp_max_failures(
                std::env::var("VICTAURI_MAX_FAILURES").ok().as_deref(),
            ),
            on_failure_cmd: std::env::var("VICTAURI_ON_FAILURE").ok(),
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
        println!("USAGE: victauri-watchdog [PORT]\n");
        println!("OPTIONS:");
        println!("  -h, --help       Print help");
        println!("  -V, --version    Print version\n");
        println!("ENVIRONMENT:");
        println!("  VICTAURI_PORT           Server port (default: 7373)");
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
    let url = format!("http://127.0.0.1:{}/health", config.port);

    tracing::info!(
        port = config.port,
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
    let status = if cfg!(windows) {
        tokio::process::Command::new("cmd")
            .args(["/C", cmd])
            .status()
            .await?
    } else {
        tokio::process::Command::new("sh")
            .args(["-c", cmd])
            .status()
            .await?
    };
    Ok(status)
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
        }
    }

    #[test]
    fn config_defaults() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_env();
        let config = Config::from_env();
        assert_eq!(config.port, 7373);
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
