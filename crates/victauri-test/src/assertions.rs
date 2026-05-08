use serde_json::Value;

use crate::VictauriClient;
use crate::error::TestError;

/// Fluent assertion builder for full-stack Tauri test verification.
///
/// Collects multiple checks (DOM, IPC, network, state) and reports all
/// failures together rather than stopping at the first one.
///
/// # Example
///
/// ```rust,ignore
/// let report = client.verify()
///     .has_text("Hello, World!")
///     .has_no_text("Error")
///     .ipc_was_called("greet")
///     .ipc_was_called_with("greet", json!({"name": "World"}))
///     .ipc_was_not_called("delete_account")
///     .no_console_errors()
///     .run()
///     .await
///     .unwrap();
///
/// report.assert_all_passed();
/// ```
pub struct VerifyBuilder<'a> {
    client: &'a mut VictauriClient,
    checks: Vec<Check>,
}

enum Check {
    HasText(String),
    HasNoText(String),
    IpcWasCalled(String),
    IpcWasCalledWith(String, Value),
    IpcWasNotCalled(String),
    NetworkRequest {
        method: Option<String>,
        url_contains: String,
    },
    NoNetworkRequest {
        url_contains: String,
    },
    NoConsoleErrors,
    StateMatches {
        frontend_expr: String,
        backend_state: Value,
    },
    IpcHealthy,
    NoGhostCommands,
    CoverageAbove(f64),
}

/// A single check result — pass or fail with context.
#[derive(Debug, Clone)]
pub struct CheckResult {
    /// Human-readable description of what was checked.
    pub description: String,
    /// Whether the check passed.
    pub passed: bool,
    /// Failure detail (empty if passed).
    pub detail: String,
}

/// Collection of check results from a `verify()` run.
#[derive(Debug)]
pub struct VerifyReport {
    /// Individual check results in order.
    pub results: Vec<CheckResult>,
}

impl VerifyReport {
    /// Returns true if all checks passed.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        self.results.iter().all(|r| r.passed)
    }

    /// Returns only the failed checks.
    #[must_use]
    pub fn failures(&self) -> Vec<&CheckResult> {
        self.results.iter().filter(|r| !r.passed).collect()
    }

    /// Converts this report to a `JUnit` XML [`JunitReport`](crate::reporting::JunitReport).
    #[must_use]
    pub fn to_junit(
        &self,
        suite_name: &str,
        duration: std::time::Duration,
    ) -> crate::reporting::JunitReport {
        crate::reporting::JunitReport::from_verify_report(self, suite_name, duration)
    }

    /// Panics with a formatted report if any check failed.
    ///
    /// # Panics
    ///
    /// Panics if any check in the report did not pass.
    pub fn assert_all_passed(&self) {
        if self.all_passed() {
            return;
        }
        let failures: Vec<String> = self
            .failures()
            .iter()
            .enumerate()
            .map(|(i, f)| format!("  {}. {} — {}", i + 1, f.description, f.detail))
            .collect();
        panic!(
            "verify() failed ({}/{} checks passed):\n{}",
            self.results.len() - failures.len(),
            self.results.len(),
            failures.join("\n")
        );
    }
}

impl<'a> VerifyBuilder<'a> {
    pub(crate) fn new(client: &'a mut VictauriClient) -> Self {
        Self {
            client,
            checks: Vec::new(),
        }
    }

    /// Assert that the page currently contains the given text.
    #[must_use]
    pub fn has_text(mut self, text: &str) -> Self {
        self.checks.push(Check::HasText(text.to_string()));
        self
    }

    /// Assert that the page does NOT contain the given text.
    #[must_use]
    pub fn has_no_text(mut self, text: &str) -> Self {
        self.checks.push(Check::HasNoText(text.to_string()));
        self
    }

    /// Assert that an IPC command was called at least once.
    #[must_use]
    pub fn ipc_was_called(mut self, command: &str) -> Self {
        self.checks.push(Check::IpcWasCalled(command.to_string()));
        self
    }

    /// Assert that an IPC command was called with specific arguments.
    #[must_use]
    pub fn ipc_was_called_with(mut self, command: &str, args: Value) -> Self {
        self.checks
            .push(Check::IpcWasCalledWith(command.to_string(), args));
        self
    }

    /// Assert that an IPC command was never called.
    #[must_use]
    pub fn ipc_was_not_called(mut self, command: &str) -> Self {
        self.checks
            .push(Check::IpcWasNotCalled(command.to_string()));
        self
    }

    /// Assert a network request was made matching the given URL substring.
    #[must_use]
    pub fn network_request(mut self, method: Option<&str>, url_contains: &str) -> Self {
        self.checks.push(Check::NetworkRequest {
            method: method.map(String::from),
            url_contains: url_contains.to_string(),
        });
        self
    }

    /// Assert NO network request was made matching the given URL substring.
    #[must_use]
    pub fn no_network_request(mut self, url_contains: &str) -> Self {
        self.checks.push(Check::NoNetworkRequest {
            url_contains: url_contains.to_string(),
        });
        self
    }

    /// Assert that no console errors were logged.
    #[must_use]
    pub fn no_console_errors(mut self) -> Self {
        self.checks.push(Check::NoConsoleErrors);
        self
    }

    /// Assert that frontend state matches backend state.
    #[must_use]
    pub fn state_matches(mut self, frontend_expr: &str, backend_state: Value) -> Self {
        self.checks.push(Check::StateMatches {
            frontend_expr: frontend_expr.to_string(),
            backend_state,
        });
        self
    }

    /// Assert that IPC integrity is healthy (no stale/errored calls).
    #[must_use]
    pub fn ipc_healthy(mut self) -> Self {
        self.checks.push(Check::IpcHealthy);
        self
    }

    /// Assert that there are no ghost commands.
    #[must_use]
    pub fn no_ghost_commands(mut self) -> Self {
        self.checks.push(Check::NoGhostCommands);
        self
    }

    /// Assert that IPC command coverage meets the given threshold percentage.
    ///
    /// Queries the command registry and IPC log to compute how many registered
    /// commands have been exercised, then checks whether the coverage percentage
    /// is at or above `threshold`.
    #[must_use]
    pub fn coverage_above(mut self, threshold: f64) -> Self {
        self.checks.push(Check::CoverageAbove(threshold));
        self
    }

    /// Execute all queued checks and return the report.
    ///
    /// # Errors
    ///
    /// Returns [`TestError`] only on transport/connection failures.
    /// Check failures are reported in the [`VerifyReport`], not as errors.
    pub async fn run(self) -> Result<VerifyReport, TestError> {
        let client = self.client;
        let mut results = Vec::with_capacity(self.checks.len());

        for check in self.checks {
            let result = run_check(client, &check).await?;
            results.push(result);
        }

        Ok(VerifyReport { results })
    }
}

async fn run_check(client: &mut VictauriClient, check: &Check) -> Result<CheckResult, TestError> {
    match check {
        Check::HasText(text) => {
            let snap = client.dom_snapshot().await?;
            let snap_str = serde_json::to_string(&snap).unwrap_or_default();
            let found = snap_str.contains(text.as_str());
            Ok(CheckResult {
                description: format!("page contains \"{text}\""),
                passed: found,
                detail: if found {
                    String::new()
                } else {
                    format!("text \"{text}\" not found in DOM")
                },
            })
        }
        Check::HasNoText(text) => {
            let snap = client.dom_snapshot().await?;
            let snap_str = serde_json::to_string(&snap).unwrap_or_default();
            let found = snap_str.contains(text.as_str());
            Ok(CheckResult {
                description: format!("page does NOT contain \"{text}\""),
                passed: !found,
                detail: if found {
                    format!("text \"{text}\" was found in DOM but shouldn't be")
                } else {
                    String::new()
                },
            })
        }
        Check::IpcWasCalled(command) => {
            let log = client.get_ipc_log(None).await?;
            let found = ipc_log_contains_command(&log, command);
            Ok(CheckResult {
                description: format!("IPC command \"{command}\" was called"),
                passed: found,
                detail: if found {
                    String::new()
                } else {
                    format!("command \"{command}\" not found in IPC log")
                },
            })
        }
        Check::IpcWasCalledWith(command, expected_args) => {
            let log = client.get_ipc_log(None).await?;
            let (found, actual_args) = ipc_log_find_with_args(&log, command, expected_args);
            Ok(CheckResult {
                description: format!("IPC \"{command}\" called with {expected_args}"),
                passed: found,
                detail: if found {
                    String::new()
                } else if let Some(actual) = actual_args {
                    format!("command called but with args: {actual}")
                } else {
                    format!("command \"{command}\" not found in IPC log")
                },
            })
        }
        Check::IpcWasNotCalled(command) => {
            let log = client.get_ipc_log(None).await?;
            let found = ipc_log_contains_command(&log, command);
            Ok(CheckResult {
                description: format!("IPC command \"{command}\" was NOT called"),
                passed: !found,
                detail: if found {
                    format!("command \"{command}\" WAS called but shouldn't have been")
                } else {
                    String::new()
                },
            })
        }
        Check::NetworkRequest {
            method,
            url_contains,
        } => {
            let log = client.logs("network", None).await?;
            let found = network_log_matches(&log, method.as_deref(), url_contains);
            let desc = match method {
                Some(m) => format!("network {m} request to \"*{url_contains}*\""),
                None => format!("network request to \"*{url_contains}*\""),
            };
            Ok(CheckResult {
                description: desc,
                passed: found,
                detail: if found {
                    String::new()
                } else {
                    "no matching network request found".to_string()
                },
            })
        }
        Check::NoNetworkRequest { url_contains } => {
            let log = client.logs("network", None).await?;
            let found = network_log_matches(&log, None, url_contains);
            Ok(CheckResult {
                description: format!("NO network request to \"*{url_contains}*\""),
                passed: !found,
                detail: if found {
                    format!("found network request matching \"{url_contains}\" but shouldn't have")
                } else {
                    String::new()
                },
            })
        }
        Check::NoConsoleErrors => {
            let log = client.logs("console", None).await?;
            let errors = console_log_errors(&log);
            Ok(CheckResult {
                description: "no console errors".to_string(),
                passed: errors.is_empty(),
                detail: if errors.is_empty() {
                    String::new()
                } else {
                    format!("{} error(s): {}", errors.len(), errors.join("; "))
                },
            })
        }
        Check::StateMatches {
            frontend_expr,
            backend_state,
        } => {
            let result = client
                .verify_state(frontend_expr, backend_state.clone())
                .await?;
            let passed = result
                .get("passed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(CheckResult {
                description: format!("state matches ({frontend_expr})"),
                passed,
                detail: if passed {
                    String::new()
                } else {
                    let divs = result.get("divergences").cloned().unwrap_or(Value::Null);
                    format!("divergences: {divs}")
                },
            })
        }
        Check::IpcHealthy => {
            let result = client.check_ipc_integrity().await?;
            let healthy = result
                .get("healthy")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(CheckResult {
                description: "IPC integrity healthy".to_string(),
                passed: healthy,
                detail: if healthy {
                    String::new()
                } else {
                    serde_json::to_string(&result).unwrap_or_default()
                },
            })
        }
        Check::NoGhostCommands => {
            let result = client.detect_ghost_commands().await?;
            let ghosts = result
                .get("ghost_commands")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            Ok(CheckResult {
                description: "no ghost commands".to_string(),
                passed: ghosts == 0,
                detail: if ghosts == 0 {
                    String::new()
                } else {
                    format!("{ghosts} ghost command(s) found")
                },
            })
        }
        Check::CoverageAbove(threshold) => {
            let report = crate::coverage::coverage_report(client).await?;
            let passed = report.meets_threshold(*threshold);
            Ok(CheckResult {
                description: format!(
                    "IPC coverage >= {threshold:.1}% (actual: {:.1}%)",
                    report.coverage_percentage
                ),
                passed,
                detail: if passed {
                    String::new()
                } else {
                    format!(
                        "coverage {:.1}% is below threshold {threshold:.1}%",
                        report.coverage_percentage
                    )
                },
            })
        }
    }
}

fn ipc_log_contains_command(log: &Value, command: &str) -> bool {
    if let Some(arr) = log.as_array() {
        return arr.iter().any(|entry| {
            entry
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|c| c == command)
        });
    }
    if let Some(entries) = log.get("entries").and_then(Value::as_array) {
        return entries.iter().any(|entry| {
            entry
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|c| c == command)
        });
    }
    false
}

fn ipc_log_find_with_args(
    log: &Value,
    command: &str,
    expected_args: &Value,
) -> (bool, Option<Value>) {
    let entries = if let Some(arr) = log.as_array() {
        arr.clone()
    } else if let Some(entries) = log.get("entries").and_then(Value::as_array) {
        entries.clone()
    } else {
        return (false, None);
    };

    let mut last_args = None;
    for entry in &entries {
        let cmd = entry.get("command").and_then(Value::as_str).unwrap_or("");
        if cmd != command {
            continue;
        }
        let args = entry.get("args").or_else(|| entry.get("request_body"));
        if let Some(args) = args {
            if args_match(args, expected_args) {
                return (true, None);
            }
            last_args = Some(args.clone());
        } else if expected_args.is_null()
            || expected_args == &Value::Object(serde_json::Map::default())
        {
            return (true, None);
        }
    }
    (false, last_args)
}

fn args_match(actual: &Value, expected: &Value) -> bool {
    match expected {
        Value::Object(exp_map) => {
            let Some(actual_map) = actual.as_object() else {
                return false;
            };
            exp_map
                .iter()
                .all(|(k, v)| actual_map.get(k).is_some_and(|av| av == v))
        }
        _ => actual == expected,
    }
}

fn network_log_matches(log: &Value, method: Option<&str>, url_contains: &str) -> bool {
    let entries = if let Some(arr) = log.as_array() {
        arr.as_slice()
    } else if let Some(entries) = log.get("entries").and_then(Value::as_array) {
        entries.as_slice()
    } else {
        return false;
    };

    entries.iter().any(|entry| {
        let url = entry.get("url").and_then(Value::as_str).unwrap_or("");
        if !url.contains(url_contains) {
            return false;
        }
        if let Some(m) = method {
            let req_method = entry.get("method").and_then(Value::as_str).unwrap_or("");
            return req_method.eq_ignore_ascii_case(m);
        }
        true
    })
}

fn console_log_errors(log: &Value) -> Vec<String> {
    let entries = if let Some(arr) = log.as_array() {
        arr.as_slice()
    } else if let Some(entries) = log.get("entries").and_then(Value::as_array) {
        entries.as_slice()
    } else {
        return Vec::new();
    };

    entries
        .iter()
        .filter_map(|entry| {
            let level = entry.get("level").and_then(Value::as_str).unwrap_or("");
            if level == "error" {
                let msg = entry
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("(no message)")
                    .to_string();
                Some(msg)
            } else {
                None
            }
        })
        .collect()
}

// ── Standalone IPC assertion functions ──────────────────────────────────────

/// Assert that a specific IPC command was called at least once.
///
/// # Panics
///
/// Panics if the command was not found in the IPC log.
///
/// # Examples
///
/// ```
/// use serde_json::json;
///
/// let log = json!([{"command": "save_settings", "args": {"theme": "dark"}}]);
/// victauri_test::assert_ipc_called(&log, "save_settings");
/// ```
pub fn assert_ipc_called(log: &Value, command: &str) {
    assert!(
        ipc_log_contains_command(log, command),
        "expected IPC command \"{command}\" to have been called, but it was not found in the log"
    );
}

/// Assert that a specific IPC command was called with the given arguments.
///
/// Uses partial matching — the expected args only need to be a subset of actual args.
///
/// # Panics
///
/// Panics if the command was not called with matching arguments.
///
/// # Examples
///
/// ```
/// use serde_json::json;
///
/// let log = json!([{"command": "save_settings", "args": {"theme": "dark", "lang": "en"}}]);
/// victauri_test::assert_ipc_called_with(&log, "save_settings", &json!({"theme": "dark"}));
/// ```
pub fn assert_ipc_called_with(log: &Value, command: &str, expected_args: &Value) {
    let (found, actual_args) = ipc_log_find_with_args(log, command, expected_args);
    if !found {
        if let Some(actual) = actual_args {
            panic!(
                "IPC command \"{command}\" was called but with different args:\n  expected: {expected_args}\n  actual:   {actual}"
            );
        } else {
            panic!("IPC command \"{command}\" was never called (expected args: {expected_args})");
        }
    }
}

/// Assert that a specific IPC command was NOT called.
///
/// # Panics
///
/// Panics if the command was found in the IPC log.
///
/// # Examples
///
/// ```
/// use serde_json::json;
///
/// let log = json!([{"command": "save_settings", "args": {}}]);
/// victauri_test::assert_ipc_not_called(&log, "delete_account");
/// ```
pub fn assert_ipc_not_called(log: &Value, command: &str) {
    assert!(
        !ipc_log_contains_command(log, command),
        "expected IPC command \"{command}\" to NOT have been called, but it was"
    );
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn ipc_contains_finds_command_in_array() {
        let log = json!([
            {"command": "greet", "args": {"name": "World"}},
            {"command": "save_settings", "args": {"theme": "dark"}}
        ]);
        assert!(ipc_log_contains_command(&log, "greet"));
        assert!(ipc_log_contains_command(&log, "save_settings"));
        assert!(!ipc_log_contains_command(&log, "delete_account"));
    }

    #[test]
    fn ipc_contains_finds_command_in_entries_object() {
        let log = json!({"entries": [{"command": "fetch_data"}]});
        assert!(ipc_log_contains_command(&log, "fetch_data"));
        assert!(!ipc_log_contains_command(&log, "nope"));
    }

    #[test]
    fn args_match_partial_object() {
        let actual = json!({"theme": "dark", "lang": "en", "notifications": true});
        let expected = json!({"theme": "dark"});
        assert!(args_match(&actual, &expected));
    }

    #[test]
    fn args_match_full_object() {
        let actual = json!({"theme": "dark"});
        let expected = json!({"theme": "dark"});
        assert!(args_match(&actual, &expected));
    }

    #[test]
    fn args_match_fails_on_mismatch() {
        let actual = json!({"theme": "light"});
        let expected = json!({"theme": "dark"});
        assert!(!args_match(&actual, &expected));
    }

    #[test]
    fn args_match_scalar() {
        assert!(args_match(&json!("hello"), &json!("hello")));
        assert!(!args_match(&json!("hello"), &json!("world")));
    }

    #[test]
    fn ipc_find_with_args_partial_match() {
        let log = json!([
            {"command": "save", "args": {"theme": "dark", "lang": "en"}}
        ]);
        let (found, _) = ipc_log_find_with_args(&log, "save", &json!({"theme": "dark"}));
        assert!(found);
    }

    #[test]
    fn ipc_find_with_args_no_match_returns_actual() {
        let log = json!([
            {"command": "save", "args": {"theme": "light"}}
        ]);
        let (found, actual) = ipc_log_find_with_args(&log, "save", &json!({"theme": "dark"}));
        assert!(!found);
        assert_eq!(actual, Some(json!({"theme": "light"})));
    }

    #[test]
    fn ipc_find_with_args_command_not_found() {
        let log = json!([{"command": "other", "args": {}}]);
        let (found, actual) = ipc_log_find_with_args(&log, "save", &json!({"theme": "dark"}));
        assert!(!found);
        assert_eq!(actual, None);
    }

    #[test]
    fn network_log_matches_url() {
        let log = json!([
            {"url": "http://api.example.com/users", "method": "GET", "status": 200},
            {"url": "http://api.example.com/settings", "method": "POST", "status": 201}
        ]);
        assert!(network_log_matches(&log, None, "/users"));
        assert!(network_log_matches(&log, Some("POST"), "/settings"));
        assert!(!network_log_matches(&log, Some("DELETE"), "/settings"));
        assert!(!network_log_matches(&log, None, "/nonexistent"));
    }

    #[test]
    fn console_errors_filters_by_level() {
        let log = json!([
            {"level": "log", "message": "info msg"},
            {"level": "error", "message": "something broke"},
            {"level": "warn", "message": "careful"},
            {"level": "error", "message": "another error"}
        ]);
        let errors = console_log_errors(&log);
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0], "something broke");
        assert_eq!(errors[1], "another error");
    }

    #[test]
    fn console_errors_empty_for_no_errors() {
        let log = json!([{"level": "log", "message": "all good"}]);
        assert!(console_log_errors(&log).is_empty());
    }

    #[test]
    fn assert_ipc_called_passes() {
        let log = json!([{"command": "greet", "args": {"name": "World"}}]);
        assert_ipc_called(&log, "greet");
    }

    #[test]
    #[should_panic(expected = "was not found in the log")]
    fn assert_ipc_called_fails() {
        let log = json!([{"command": "greet", "args": {}}]);
        assert_ipc_called(&log, "nonexistent");
    }

    #[test]
    fn assert_ipc_called_with_passes() {
        let log = json!([{"command": "save", "args": {"theme": "dark", "extra": true}}]);
        assert_ipc_called_with(&log, "save", &json!({"theme": "dark"}));
    }

    #[test]
    #[should_panic(expected = "different args")]
    fn assert_ipc_called_with_fails_wrong_args() {
        let log = json!([{"command": "save", "args": {"theme": "light"}}]);
        assert_ipc_called_with(&log, "save", &json!({"theme": "dark"}));
    }

    #[test]
    fn assert_ipc_not_called_passes() {
        let log = json!([{"command": "greet", "args": {}}]);
        assert_ipc_not_called(&log, "delete_everything");
    }

    #[test]
    #[should_panic(expected = "NOT have been called")]
    fn assert_ipc_not_called_fails() {
        let log = json!([{"command": "greet", "args": {}}]);
        assert_ipc_not_called(&log, "greet");
    }

    #[test]
    fn verify_report_all_passed() {
        let report = VerifyReport {
            results: vec![
                CheckResult {
                    description: "check1".into(),
                    passed: true,
                    detail: String::new(),
                },
                CheckResult {
                    description: "check2".into(),
                    passed: true,
                    detail: String::new(),
                },
            ],
        };
        assert!(report.all_passed());
        assert!(report.failures().is_empty());
    }

    #[test]
    fn verify_report_with_failures() {
        let report = VerifyReport {
            results: vec![
                CheckResult {
                    description: "pass".into(),
                    passed: true,
                    detail: String::new(),
                },
                CheckResult {
                    description: "fail".into(),
                    passed: false,
                    detail: "something wrong".into(),
                },
            ],
        };
        assert!(!report.all_passed());
        assert_eq!(report.failures().len(), 1);
        assert_eq!(report.failures()[0].description, "fail");
    }

    #[test]
    #[should_panic(expected = "verify() failed")]
    fn verify_report_assert_panics_on_failure() {
        let report = VerifyReport {
            results: vec![CheckResult {
                description: "bad".into(),
                passed: false,
                detail: "it broke".into(),
            }],
        };
        report.assert_all_passed();
    }
}
