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
#[non_exhaustive]
pub struct CheckResult {
    /// Human-readable description of what was checked.
    pub description: String,
    /// Whether the check passed.
    pub passed: bool,
    /// Failure detail. Conventionally empty if passed, but a passing check may carry a note (for
    /// example a skip reason, or candidates that were deliberately not treated as failures).
    pub detail: String,
    /// The check did not run because its precondition was not met. A skipped check counts as
    /// passed; a `JUnit` report writes it as `<skipped>`.
    pub skipped: bool,
}

impl CheckResult {
    /// Creates a check result (`detail` is conventionally empty when `passed`).
    #[must_use]
    pub fn new(description: impl Into<String>, passed: bool, detail: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            passed,
            detail: detail.into(),
            skipped: false,
        }
    }
}

/// Collection of check results from a `verify()` run.
#[derive(Debug)]
#[non_exhaustive]
pub struct VerifyReport {
    /// Individual check results in order.
    pub results: Vec<CheckResult>,
}

impl Check {
    /// What this check asserts. Also the description of a check that could not run.
    fn description(&self) -> String {
        match self {
            Self::HasText(text) => format!("page contains \"{text}\""),
            Self::HasNoText(text) => format!("page does NOT contain \"{text}\""),
            Self::IpcWasCalled(command) => format!("IPC command \"{command}\" was called"),
            Self::IpcWasCalledWith(command, args) => {
                format!("IPC \"{command}\" called with {args}")
            }
            Self::IpcWasNotCalled(command) => format!("IPC command \"{command}\" was NOT called"),
            Self::NetworkRequest {
                method: Some(m),
                url_contains,
            } => format!("network {m} request to \"*{url_contains}*\""),
            Self::NetworkRequest {
                method: None,
                url_contains,
            } => format!("network request to \"*{url_contains}*\""),
            Self::NoNetworkRequest { url_contains } => {
                format!("NO network request to \"*{url_contains}*\"")
            }
            Self::NoConsoleErrors => "no console errors".to_string(),
            Self::StateMatches { frontend_expr, .. } => format!("state matches ({frontend_expr})"),
            Self::IpcHealthy => "IPC integrity healthy".to_string(),
            Self::NoGhostCommands => "no ghost commands".to_string(),
            Self::CoverageAbove(threshold) => format!("IPC coverage >= {threshold:.1}%"),
        }
    }
}

impl VerifyReport {
    /// Creates a report from individual check results (e.g. to export custom
    /// checks through [`Self::to_junit`]).
    #[must_use]
    pub fn new(results: Vec<CheckResult>) -> Self {
        Self { results }
    }

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
            // Descriptions and details can carry page text (console errors, DOM text, state
            // values): render each single-line so it cannot drive the terminal or start a
            // `::error` workflow command in a CI log (R5-TERM2, as the smoke summary does).
            .map(|(i, f)| {
                format!(
                    "  {}. {} — {}",
                    i + 1,
                    crate::terminal::single_line(&f.description),
                    crate::terminal::single_line(&f.detail)
                )
            })
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

    /// Assert that the page currently contains the given text: the text a user can read, as
    /// [`VictauriClient::expect_text`] sees it, joined across elements and including shadow roots
    /// and same-origin frames.
    #[must_use]
    pub fn has_text(mut self, text: &str) -> Self {
        self.checks.push(Check::HasText(text.to_string()));
        self
    }

    /// Assert that the page does NOT contain the given text (see [`Self::has_text`]).
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
    ///
    /// The bridge keeps the newest 1000 network entries, IPC included. Once that log is full an
    /// older call may have rolled off, so this check then fails as inconclusive rather than pass on
    /// what it cannot see. To assert on a busy app, use [`VictauriClient::create_ipc_checkpoint`]
    /// and [`VictauriClient::get_ipc_calls_since`]. It also fails when Victauri reports it cannot
    /// observe the page's IPC at all (`ipc_capture` `none`).
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

    /// Assert NO network request was made matching the given URL substring. Inconclusive, and so
    /// failed, once the 1000-entry log is full (see [`Self::ipc_was_not_called`]).
    #[must_use]
    pub fn no_network_request(mut self, url_contains: &str) -> Self {
        self.checks.push(Check::NoNetworkRequest {
            url_contains: url_contains.to_string(),
        });
        self
    }

    /// Assert that no console errors were logged. Inconclusive, and so failed, once the 1000-entry
    /// console log is full (see [`Self::ipc_was_not_called`]) and it shows no error.
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

    /// Assert that IPC integrity is healthy: no call was left without a response. A command that
    /// returned an error is a sound round trip, so errored calls do not make this fail; use
    /// [`Self::no_console_errors`] or the `error_count` of `check_ipc_integrity` for those. Also
    /// fails when Victauri reports it cannot observe the page's IPC at all.
    #[must_use]
    pub fn ipc_healthy(mut self) -> Self {
        self.checks.push(Check::IpcHealthy);
        self
    }

    /// Assert that there are no confirmed ghost commands: commands the frontend invoked that only
    /// ever failed with "command not found". Weaker candidates (`frontend_only`, which may be real
    /// commands that merely returned an error) are named in the detail but do not fail the check.
    /// Fails when Victauri reports it cannot observe the page's IPC at all (`ipc_capture` `none`).
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
    /// Check failures are reported in the [`VerifyReport`], not as errors. So is a check that could
    /// not run (a tool error, a read over the result cap): it fails, with the reason as its detail,
    /// and the other checks still report.
    pub async fn run(self) -> Result<VerifyReport, TestError> {
        let client = self.client;
        let mut results = Vec::with_capacity(self.checks.len());

        for check in self.checks {
            let result = match run_check(client, &check).await {
                Ok(result) => result,
                Err(e @ (TestError::Connection { .. } | TestError::Request(_))) => return Err(e),
                Err(e) => CheckResult::new(
                    check.description(),
                    false,
                    format!(
                        "the check could not run: {}",
                        crate::terminal::single_line(&e.to_string())
                    ),
                ),
            };
            results.push(result);
        }

        Ok(VerifyReport { results })
    }
}

async fn run_check(client: &mut VictauriClient, check: &Check) -> Result<CheckResult, TestError> {
    match check {
        Check::HasText(text) => {
            let found = page_text(client).await?.contains(text.as_str());
            Ok(CheckResult {
                description: check.description(),
                passed: found,
                detail: if found {
                    String::new()
                } else {
                    format!("text \"{text}\" not found on the page")
                },
                skipped: false,
            })
        }
        Check::HasNoText(text) => {
            let found = page_text(client).await?.contains(text.as_str());
            Ok(CheckResult {
                description: check.description(),
                passed: !found,
                detail: if found {
                    format!("text \"{text}\" was found on the page but shouldn't be")
                } else {
                    String::new()
                },
                skipped: false,
            })
        }
        Check::IpcWasCalled(command) => {
            let log = client.logs_without_bodies("ipc", LOG_WINDOW).await?;
            let found = ipc_log_contains_command(&log, command);
            Ok(CheckResult {
                description: check.description(),
                passed: found,
                detail: if found {
                    String::new()
                } else {
                    format!("command \"{command}\" not found in IPC log")
                },
                skipped: false,
            })
        }
        Check::IpcWasCalledWith(command, expected_args) => {
            let log = client.get_ipc_log(Some(LOG_WINDOW)).await?;
            let (found, actual_args) = ipc_log_find_with_args(&log, command, expected_args);
            Ok(CheckResult {
                description: check.description(),
                passed: found,
                detail: if found {
                    String::new()
                } else if ipc_log_args_truncated(&log, command) {
                    format!(
                        "command \"{command}\" was called, but the log truncated its args, so \
                         they could not be compared"
                    )
                } else if let Some(actual) = actual_args {
                    format!("command called but with args: {actual}")
                } else {
                    format!("command \"{command}\" not found in IPC log")
                },
                skipped: false,
            })
        }
        Check::IpcWasNotCalled(command) => {
            let log = client.logs_without_bodies("ipc", LOG_WINDOW).await?;
            let found = ipc_log_contains_command(&log, command);
            let blind = if found {
                None
            } else {
                ipc_blind_note(client).await?
            };
            let inconclusive = !found && blind.is_none() && network_log_is_full(client, &log).await;
            Ok(CheckResult {
                description: check.description(),
                passed: !found && blind.is_none() && !inconclusive,
                detail: if found {
                    format!("command \"{command}\" WAS called but shouldn't have been")
                } else if let Some(note) = blind {
                    note
                } else if inconclusive {
                    log_full_detail("IPC")
                } else {
                    String::new()
                },
                skipped: false,
            })
        }
        Check::NetworkRequest {
            method,
            url_contains,
        } => {
            let log = client.logs_without_bodies("network", LOG_WINDOW).await?;
            let found = network_log_matches(&log, method.as_deref(), url_contains);
            Ok(CheckResult {
                description: check.description(),
                passed: found,
                detail: if found {
                    String::new()
                } else {
                    "no matching network request found".to_string()
                },
                skipped: false,
            })
        }
        Check::NoNetworkRequest { url_contains } => {
            let log = client.logs_without_bodies("network", LOG_WINDOW).await?;
            let found = network_log_matches(&log, None, url_contains);
            let inconclusive = !found && log_entry_count(&log) >= LOG_WINDOW;
            Ok(CheckResult {
                description: check.description(),
                passed: !found && !inconclusive,
                detail: if found {
                    format!("found network request matching \"{url_contains}\" but shouldn't have")
                } else if inconclusive {
                    log_full_detail("network")
                } else {
                    String::new()
                },
                skipped: false,
            })
        }
        Check::NoConsoleErrors => {
            let log = client.logs("console", Some(LOG_WINDOW)).await?;
            let errors = console_log_errors(&log);
            let inconclusive = errors.is_empty() && log_entry_count(&log) >= LOG_WINDOW;
            Ok(CheckResult {
                description: check.description(),
                passed: errors.is_empty() && !inconclusive,
                detail: if inconclusive {
                    console_log_full_detail()
                } else if errors.is_empty() {
                    String::new()
                } else {
                    format!("{} error(s): {}", errors.len(), errors.join("; "))
                },
                skipped: false,
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
                description: check.description(),
                passed,
                detail: if passed {
                    String::new()
                } else {
                    let divs = result.get("divergences").cloned().unwrap_or(Value::Null);
                    format!("divergences: {divs}")
                },
                skipped: false,
            })
        }
        Check::IpcHealthy => {
            let result = client.check_ipc_integrity().await?;
            let healthy = crate::client::ipc_integrity_is_healthy(&result);
            Ok(CheckResult {
                description: check.description(),
                passed: healthy,
                detail: if healthy {
                    String::new()
                } else {
                    serde_json::to_string(&result).unwrap_or_default()
                },
                skipped: false,
            })
        }
        Check::NoGhostCommands => {
            let result = client.detect_ghost_commands().await?;
            // `confirmed_ghosts` is the bug list: invoked, never succeeded, "command not found".
            // A plugin too old to report it fails closed rather than pass unseen.
            let Some(confirmed) = result.get("confirmed_ghosts").and_then(Value::as_array) else {
                return Ok(CheckResult {
                    description: check.description(),
                    passed: false,
                    detail: "the plugin's report has no `confirmed_ghosts`; \
                             upgrade victauri-plugin to run this check"
                        .to_string(),
                    skipped: false,
                });
            };
            // `frontend_only` is a candidate list by the tool's own description ("confirm before
            // filing"): a real command that only returned a domain error lands there even when
            // `reliability` is high, which only means the registry is not empty.
            let candidates = command_names(result.get("frontend_only"));
            let capture = result
                .pointer("/ipc_capture/capture")
                .and_then(Value::as_str);
            Ok(CheckResult {
                description: check.description(),
                passed: confirmed.is_empty() && capture != Some("none"),
                detail: if !confirmed.is_empty() {
                    format!(
                        "{} confirmed ghost command(s): {}",
                        confirmed.len(),
                        command_names(Some(&Value::Array(confirmed.clone()))).join(", ")
                    )
                } else if capture == Some("none") {
                    capture_note(&result)
                } else if capture == Some("requests_only") {
                    "IPC capture on this page is `requests_only`: calls are seen but not their \
                     outcomes, so a ghost cannot be confirmed"
                        .to_string()
                } else if !candidates.is_empty() {
                    format!(
                        "{} unconfirmed candidate(s), not treated as ghosts: {}",
                        candidates.len(),
                        candidates.join(", ")
                    )
                } else {
                    String::new()
                },
                skipped: false,
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
                skipped: false,
            })
        }
    }
}

/// The most entries the bridge's network log (which IPC calls live in) and console log hold, and
/// so the most a log read can return. The log tools default to the newest 100 when not told.
pub const LOG_WINDOW: usize = 1000;

/// The text a user can read on the page: the bridge's `getPageText`, the read `expect_text` polls,
/// or the same read done here on a plugin too old to have it. A `null` means there is no page to
/// read, which must not pass as "text not found" (`getPageText` alone answers '' there).
async fn page_text(client: &mut VictauriClient) -> Result<String, TestError> {
    match client.eval_js(PAGE_TEXT_JS).await? {
        Value::String(text) => Ok(text),
        other => Err(TestError::Other(format!(
            "could not read the page's text (is the page loaded?): {}",
            crate::terminal::single_line(&other.to_string())
        ))),
    }
}

const PAGE_TEXT_JS: &str = "(function() { \
         if (!document.body) return null; \
         var bridge = window.__VICTAURI__; \
         if (bridge && bridge.getPageText) return bridge.getPageText(); \
         function read(root) { \
           var text = (typeof root.innerText === 'string' ? root.innerText : root.textContent) || ''; \
           var els = root.querySelectorAll('*'); \
           for (var i = 0; i < els.length; i++) { \
             if (els[i].shadowRoot) text += ' ' + read(els[i].shadowRoot); \
             if (els[i].tagName === 'IFRAME' || els[i].tagName === 'FRAME') { \
               try { var doc = els[i].contentDocument; \
                     if (doc && doc.body) text += ' ' + read(doc.body); } catch (e) {} \
             } \
           } \
           return text; \
         } \
         return read(document.body); })()";

/// Why a negative IPC check means nothing here, when Victauri reports it cannot observe this page's
/// IPC. A plugin without the report (or that refuses the tool) has nothing to say.
async fn ipc_blind_note(client: &mut VictauriClient) -> Result<Option<String>, TestError> {
    let integrity = match client.check_ipc_integrity().await {
        Ok(integrity) => integrity,
        Err(TestError::ToolError(_) | TestError::Mcp { .. }) => return Ok(None),
        Err(other) => return Err(other),
    };
    let blind = integrity
        .pointer("/ipc_capture/capture")
        .and_then(Value::as_str)
        == Some("none");
    Ok(blind.then(|| capture_note(&integrity)))
}

/// A report's `ipc_capture.note`, safe to print: it can quote nothing from the page today, but
/// every other detail here is treated this way.
fn capture_note(report: &Value) -> String {
    let note = report
        .pointer("/ipc_capture/note")
        .and_then(Value::as_str)
        .unwrap_or("");
    format!(
        "inconclusive: Victauri cannot observe this page's IPC (capture: none). {}",
        crate::terminal::single_line(note)
    )
}

fn log_entry_count(log: &Value) -> usize {
    log.as_array()
        .or_else(|| log.get("entries").and_then(Value::as_array))
        .map_or(0, Vec::len)
}

/// Whether older entries may have rolled off the log `ipc_log` was read from. IPC calls share the
/// bridge's network log with other requests, so the IPC list alone can look short of a full log.
/// A failed count (eval disabled by the app's privacy profile) falls back to that list.
async fn network_log_is_full(client: &mut VictauriClient, ipc_log: &Value) -> bool {
    const COUNT_JS: &str =
        "(window.__VICTAURI__?.getNetworkLog(null, 0, { bodies: false }) || []).length";
    let network = client
        .eval_js(COUNT_JS)
        .await
        .ok()
        .and_then(|count| count.as_u64());
    log_entry_count(ipc_log) >= LOG_WINDOW
        || network.is_some_and(|count| count >= LOG_WINDOW as u64)
}

fn log_full_detail(which: &str) -> String {
    format!(
        "inconclusive: the {which} log holds its maximum of {LOG_WINDOW} entries, so an older \
         match may have rolled off. On a busy app, take a checkpoint first \
         (create_ipc_checkpoint / get_ipc_calls_since)"
    )
}

/// A console log holding its maximum: the reason an absence in it proves nothing.
pub fn console_log_full_detail() -> String {
    format!(
        "inconclusive: the console log holds its maximum of {LOG_WINDOW} entries, so an older error \
         may have rolled off. On a chatty app, clear the log first (logs {{action:\"clear\"}})"
    )
}

/// The `name`s in a list of ghost report entries, safe to print (they come from the page).
fn command_names(list: Option<&Value>) -> Vec<String> {
    list.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("name").and_then(Value::as_str))
        .map(crate::terminal::single_line)
        .collect()
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

/// Whether the log could not keep some call's arguments whole, so a mismatch proves nothing.
fn ipc_log_args_truncated(log: &Value, command: &str) -> bool {
    log.as_array()
        .or_else(|| log.get("entries").and_then(Value::as_array))
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("command").and_then(Value::as_str) == Some(command))
        .any(|entry| {
            entry.get("args_truncated").and_then(Value::as_bool) == Some(true)
                || entry
                    .get("args")
                    .and_then(Value::as_str)
                    .is_some_and(|args| args.starts_with("[truncated "))
        })
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
                // Page-controlled: neutralize before it lands in a check detail that is printed,
                // joined with "; " and put into panics (R5-TERM2).
                let msg = entry
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("(no message)");
                Some(crate::terminal::single_line(msg))
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
        if ipc_log_args_truncated(log, command) {
            panic!(
                "IPC command \"{command}\" was called, but the log truncated its args, so they \
                 could not be compared (expected: {expected_args})"
            );
        } else if let Some(actual) = actual_args {
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
/// This judges only the `log` it is given, so read it with an explicit limit:
/// `client.get_ipc_log(Some(1000))`. With `None` the tool returns only the newest 100 calls.
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

    /// `PAGE_TEXT_JS` run for real in Node, against a fake document: joined shadow-root and frame
    /// text, and `null` when there is no body.
    #[test]
    fn page_text_script_runs() {
        let program = format!(
            r#"
var shadow = {{ textContent: "in shadow", querySelectorAll: function() {{ return []; }} }};
var frameBody = {{ innerText: "in frame", querySelectorAll: function() {{ return []; }} }};
var document = {{ body: {{ innerText: "top", querySelectorAll: function() {{ return [
  {{ shadowRoot: shadow, tagName: "DIV" }},
  {{ tagName: "IFRAME", contentDocument: {{ body: frameBody }} }},
  {{ tagName: "IFRAME", get contentDocument() {{ throw new Error("cross-origin"); }} }}
]; }} }} }};
var window = {{}};
console.log(JSON.stringify({PAGE_TEXT_JS}));
window.__VICTAURI__ = {{ getPageText: function() {{ return "from bridge"; }} }};
console.log(JSON.stringify({PAGE_TEXT_JS}));
document = {{ body: null }};
console.log(JSON.stringify({PAGE_TEXT_JS}));
"#
        );
        let out = match std::process::Command::new("node")
            .args(["-e", &program])
            .output()
        {
            Ok(out) => out,
            Err(e) => {
                eprintln!("skipping: node is not available ({e})");
                return;
            }
        };
        assert!(
            out.status.success(),
            "node failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(
            lines,
            ["\"top in shadow in frame\"", "\"from bridge\"", "null"]
        );
    }

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

    /// `trimField` turns over-long args into this text, and lane A's `args_truncated` marks it.
    #[test]
    fn truncated_args_are_not_reported_as_a_mismatch() {
        let marked = json!([{"command": "save", "args": {}, "args_truncated": true}]);
        let marker = json!([{"command": "save", "args": "[truncated 5000 bytes]"}]);
        let whole = json!([{"command": "save", "args": {"theme": "light"}}]);
        assert!(ipc_log_args_truncated(&marked, "save"));
        assert!(ipc_log_args_truncated(&marker, "save"));
        assert!(!ipc_log_args_truncated(&whole, "save"));
        assert!(!ipc_log_args_truncated(&marked, "other"));

        let payload = std::panic::catch_unwind(|| {
            assert_ipc_called_with(&marked, "save", &json!({"theme": "dark"}));
        })
        .unwrap_err();
        let msg = payload
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(msg.contains("truncated"), "{msg}");
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

    /// R5-TERM2: console.error text is page-controlled. It must not carry control
    /// characters / ANSI escapes, or a newline that starts a `::error` GitHub workflow
    /// command, into the check detail — which is printed and put into panics.
    #[test]
    fn console_errors_are_neutralized_for_terminals() {
        let log = json!([
            {"level": "error", "message": "boom\n::error file=src/main.rs::forged\u{1b}[2J\u{202e}"}
        ]);
        let errors = console_log_errors(&log);
        assert_eq!(errors.len(), 1);
        let e = &errors[0];
        assert!(!e.chars().any(char::is_control), "{e:?}");
        assert!(!e.contains('\u{202e}'), "{e:?}");
        assert!(
            e.contains("boom") && e.contains("forged"),
            "text is kept visibly: {e}"
        );
    }

    /// R5-TERM2: `assert_all_passed` panics with check details, which may carry page text
    /// from any check — render them single-line, like the smoke summary (R4-TERM1).
    #[test]
    fn assert_all_passed_panic_message_is_terminal_safe() {
        let report = VerifyReport {
            results: vec![CheckResult {
                description: "desc\u{1b}[31m".into(),
                passed: false,
                detail: "page said\n::warning::forged\r\u{7}".into(),
                skipped: false,
            }],
        };
        let payload = std::panic::catch_unwind(|| report.assert_all_passed()).unwrap_err();
        let msg = payload
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(msg.starts_with("verify() failed"), "{msg}");
        assert!(
            !msg.contains('\u{1b}') && !msg.contains('\r') && !msg.contains('\u{7}'),
            "{msg:?}"
        );
        assert!(
            !msg.lines().any(|l| l.trim_start().starts_with("::")),
            "no line may start a workflow command: {msg:?}"
        );
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
                    skipped: false,
                },
                CheckResult {
                    description: "check2".into(),
                    passed: true,
                    detail: String::new(),
                    skipped: false,
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
                    skipped: false,
                },
                CheckResult {
                    description: "fail".into(),
                    passed: false,
                    detail: "something wrong".into(),
                    skipped: false,
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
                skipped: false,
            }],
        };
        report.assert_all_passed();
    }
}
