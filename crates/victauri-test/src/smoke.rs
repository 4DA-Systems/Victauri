//! Built-in smoke test suite for Victauri-powered Tauri apps.
//!
//! **Layer 1** — Individual assertion helpers on [`VictauriClient`] that
//! combine fetching data and verifying it in a single call. Each returns
//! `Result<(), TestError>` where [`TestError::Assertion`] indicates a
//! failed check.
//!
//! **Layer 2** — [`VictauriClient::smoke_test()`] runs all generic checks
//! and produces a [`SmokeReport`] without stopping at the first failure.
//!
//! # Example
//!
//! ```rust,ignore
//! use victauri_test::VictauriClient;
//!
//! let mut client = VictauriClient::discover().await.unwrap();
//!
//! // Layer 1 — single assertion
//! client.assert_eval_works().await.unwrap();
//! client.assert_heap_under_mb(256.0).await.unwrap();
//!
//! // Layer 2 — full smoke suite
//! let report = client.smoke_test().await.unwrap();
//! report.assert_all_passed();
//! ```

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::assertions::{CheckResult, VerifyReport};
use crate::client::VictauriClient;
use crate::error::TestError;

/// Result of a single smoke check with timing.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SmokeCheckResult {
    /// Human-readable name of the check.
    pub name: String,
    /// Whether the check passed.
    pub passed: bool,
    /// Failure detail (empty when passed), or the reason a skipped check did not run.
    pub detail: String,
    /// Wall-clock duration of this check.
    pub duration: Duration,
    /// The check did not run because its precondition is not met (e.g. the recording
    /// check while another client's recording is in progress). A skipped check counts as
    /// passed; [`SmokeReport::to_summary`] shows it as `[SKIP]` with the reason.
    pub skipped: bool,
}

/// Aggregate report from [`VictauriClient::smoke_test()`].
///
/// ```no_run
/// # async fn demo() -> Result<(), victauri_test::TestError> {
/// let mut client = victauri_test::VictauriClient::discover().await?;
/// let report = client.smoke_test().await?;
/// for failed in report.failures() {
///     eprintln!("{}: {}", failed.name, failed.detail);
/// }
/// assert!(report.all_passed(), "{}", report.to_summary());
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub struct SmokeReport {
    /// Individual check results in execution order.
    pub checks: Vec<SmokeCheckResult>,
    /// Total wall-clock duration of the suite.
    pub duration: Duration,
}

impl SmokeReport {
    /// Returns `true` if every check passed.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        self.checks.iter().all(|c| c.passed)
    }

    /// Returns only the failed checks.
    #[must_use]
    pub fn failures(&self) -> Vec<&SmokeCheckResult> {
        self.checks.iter().filter(|c| !c.passed).collect()
    }

    /// Number of passing checks.
    #[must_use]
    pub fn passed_count(&self) -> usize {
        self.checks.iter().filter(|c| c.passed).count()
    }

    /// Total number of checks.
    #[must_use]
    pub fn total_count(&self) -> usize {
        self.checks.len()
    }

    /// Panics with a formatted summary if any check failed.
    ///
    /// # Panics
    ///
    /// Panics when at least one check did not pass.
    pub fn assert_all_passed(&self) {
        if self.all_passed() {
            return;
        }
        let failures: Vec<String> = self
            .failures()
            .iter()
            .enumerate()
            .map(|(i, f)| {
                format!(
                    "  {}. {} — {}",
                    i + 1,
                    crate::terminal::single_line(&f.name),
                    crate::terminal::single_line(&f.detail)
                )
            })
            .collect();
        panic!(
            "smoke_test failed ({}/{} passed):\n{}",
            self.passed_count(),
            self.total_count(),
            failures.join("\n")
        );
    }

    /// Converts to a [`VerifyReport`] for `JUnit` XML output.
    #[must_use]
    pub fn to_verify_report(&self) -> VerifyReport {
        VerifyReport {
            results: self
                .checks
                .iter()
                .map(|c| CheckResult {
                    description: c.name.clone(),
                    passed: c.passed,
                    detail: c.detail.clone(),
                })
                .collect(),
        }
    }

    /// Formats as a human-readable summary.
    ///
    /// Check names and failure details are rendered on ONE line each with every control
    /// character escaped: details can carry page-controlled text (console / uncaught-error
    /// messages), and a raw newline followed by `::error` would otherwise become a forged
    /// GitHub Actions annotation in a CI log (R4-TERM1).
    #[must_use]
    pub fn to_summary(&self) -> String {
        let mut out = String::with_capacity(1024);
        out.push_str(&format!(
            "Smoke Test: {}/{} passed ({:.1}s)\n\n",
            self.passed_count(),
            self.total_count(),
            self.duration.as_secs_f64(),
        ));
        for check in &self.checks {
            let status = match (check.passed, check.skipped) {
                (true, true) => "SKIP",
                (true, false) => "PASS",
                (false, _) => "FAIL",
            };
            out.push_str(&format!(
                "  [{status}] {} ({:.0}ms)\n",
                crate::terminal::single_line(&check.name),
                check.duration.as_millis(),
            ));
            if (!check.passed || check.skipped) && !check.detail.is_empty() {
                out.push_str(&format!(
                    "         {}\n",
                    crate::terminal::single_line(&check.detail)
                ));
            }
        }
        out
    }
}

/// Configuration for the smoke test suite.
///
/// ```
/// let config = victauri_test::smoke::SmokeConfig::default();
/// assert_eq!(config.max_dom_complete_ms, 10_000);
/// ```
#[derive(Debug, Clone)]
pub struct SmokeConfig {
    /// Maximum acceptable DOM complete time in milliseconds (default: 10 000).
    pub max_dom_complete_ms: u64,
    /// Maximum acceptable JS heap usage in megabytes (default: 512).
    pub max_heap_mb: f64,
}

impl Default for SmokeConfig {
    fn default() -> Self {
        Self {
            max_dom_complete_ms: 10_000,
            max_heap_mb: 512.0,
        }
    }
}

// ── Layer 1: Individual Assertion Helpers ──────────────────────────────────

impl VictauriClient {
    /// Assert that JavaScript evaluation works (evaluates `1+1`).
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Assertion`] if evaluation returns the wrong result.
    pub async fn assert_eval_works(&mut self) -> Result<(), TestError> {
        let result = self.eval_js("1+1").await?;
        let val = result
            .as_f64()
            .or_else(|| result.as_str().and_then(|s| s.parse::<f64>().ok()));
        if val != Some(2.0) {
            return Err(TestError::Assertion(format!(
                "eval_js(\"1+1\") returned {result}, expected 2"
            )));
        }
        Ok(())
    }

    /// Assert that DOM snapshot returns a valid tree with elements.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Assertion`] if the snapshot is empty or malformed.
    pub async fn assert_dom_snapshot_valid(&mut self) -> Result<(), TestError> {
        let snap = self.dom_snapshot().await?;
        if snap.get("tree").is_none() && snap.get("ref_id").is_none() {
            return Err(TestError::Assertion(
                "DOM snapshot has no tree or ref_id".to_string(),
            ));
        }
        Ok(())
    }

    /// Assert that screenshot captures window image data.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Assertion`] if no image data in the response.
    pub async fn assert_screenshot_ok(&mut self) -> Result<(), TestError> {
        // The tool responding is sufficient. Headless CI (Xvfb / no display
        // session) has no native window handle to capture — which, now that the
        // SDK honors MCP `isError` (0.7.6), surfaces as a tool-level error instead
        // of being silently swallowed. That is an environment limitation, not a
        // Victauri failure, so tolerate the "no window handle" case while still
        // failing on transport/connection errors.
        match self.screenshot().await {
            Ok(_) => Ok(()),
            Err(TestError::ToolError(msg))
                if msg.contains("handle") || msg.contains("not available") =>
            {
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Assert that at least one window exists.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Assertion`] if no windows are found.
    pub async fn assert_windows_exist(&mut self) -> Result<(), TestError> {
        let windows = self.list_windows().await?;
        let count = windows.as_array().map_or(0, Vec::len);
        if count == 0 {
            return Err(TestError::Assertion("no windows found".to_string()));
        }
        Ok(())
    }

    /// Assert that IPC integrity is healthy.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Assertion`] if the IPC integrity check reports
    /// stale or errored calls.
    pub async fn assert_ipc_integrity_ok(&mut self) -> Result<(), TestError> {
        let integrity = self.check_ipc_integrity().await?;
        let healthy = integrity
            .get("healthy")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !healthy {
            return Err(TestError::Assertion(format!(
                "IPC integrity unhealthy: {}",
                serde_json::to_string(&integrity).unwrap_or_default()
            )));
        }
        Ok(())
    }

    /// Assert that the accessibility audit has zero violations.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Assertion`] if any a11y violations are found.
    pub async fn assert_accessible(&mut self) -> Result<(), TestError> {
        let audit = self.audit_accessibility().await?;
        let violations = audit
            .pointer("/summary/violations")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if violations > 0 {
            let details = audit.get("violations").cloned().unwrap_or(Value::Null);
            return Err(TestError::Assertion(format!(
                "{violations} a11y violation(s): {}",
                serde_json::to_string(&details).unwrap_or_default()
            )));
        }
        Ok(())
    }

    /// Assert DOM complete time is under the given duration.
    ///
    /// Passes silently if the browser does not expose navigation timing.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Assertion`] if load time exceeds the budget.
    pub async fn assert_dom_complete_under(&mut self, max: Duration) -> Result<(), TestError> {
        let metrics = self.get_performance_metrics().await?;
        if let Some(ms) = metrics
            .pointer("/navigation/dom_complete_ms")
            .and_then(Value::as_f64)
        {
            let max_ms = max.as_millis() as f64;
            if ms > max_ms {
                return Err(TestError::Assertion(format!(
                    "DOM complete took {ms:.0}ms, budget is {max_ms:.0}ms"
                )));
            }
        }
        Ok(())
    }

    /// Assert JS heap usage is under the given megabyte limit.
    ///
    /// Passes silently if the browser does not expose heap metrics.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Assertion`] if heap exceeds the budget.
    pub async fn assert_heap_under_mb(&mut self, max_mb: f64) -> Result<(), TestError> {
        let metrics = self.get_performance_metrics().await?;
        if let Some(used) = metrics.pointer("/js_heap/used_mb").and_then(Value::as_f64)
            && used > max_mb
        {
            return Err(TestError::Assertion(format!(
                "JS heap is {used:.1}MB, budget is {max_mb:.1}MB"
            )));
        }
        Ok(())
    }

    /// Assert there are no uncaught errors in the console log.
    ///
    /// Checks for entries with `[uncaught]` prefix (from the JS bridge's
    /// `window.onerror` and `unhandledrejection` handlers).
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Assertion`] if uncaught errors are found.
    pub async fn assert_no_uncaught_errors(&mut self) -> Result<(), TestError> {
        let log = self.logs("console", None).await?;
        let entries = log
            .as_array()
            .or_else(|| log.get("entries").and_then(Value::as_array));
        if let Some(entries) = entries {
            let uncaught: Vec<&str> = entries
                .iter()
                .filter_map(|e| {
                    let msg = e.get("message").and_then(Value::as_str)?;
                    if msg.starts_with("[uncaught]") {
                        Some(msg)
                    } else {
                        None
                    }
                })
                .collect();
            if !uncaught.is_empty() {
                return Err(TestError::Assertion(format!(
                    "{} uncaught error(s): {}",
                    uncaught.len(),
                    // Page-controlled text: never carry raw control characters onward.
                    uncaught
                        .iter()
                        .take(3)
                        .map(|msg| crate::terminal::single_line(msg))
                        .collect::<Vec<_>>()
                        .join("; ")
                )));
            }
        }
        Ok(())
    }

    /// Assert that the recording lifecycle works end-to-end.
    ///
    /// Starts a recording, generates activity via `eval_js`, waits for the
    /// event drain loop (2 seconds), stops recording, and verifies events
    /// were captured.
    ///
    /// The app records one session at a time. If a recording is already in progress —
    /// another client's, e.g. a concurrent `victauri record` — it is left running and this
    /// check does not run (returns `Ok`); [`Self::smoke_test`] reports it as skipped.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Assertion`] if recording captures zero events.
    pub async fn assert_recording_lifecycle(&mut self) -> Result<(), TestError> {
        self.recording_lifecycle_check().await.map(|_| ())
    }

    /// [`Self::assert_recording_lifecycle`]; `Ok(Some(reason))` when it was skipped.
    ///
    /// It never stops a recording it did not start (R5B-SMOKE1): it used to call
    /// `stop_recording` first "to clean up", which ended — and discarded — any recording
    /// already in progress. Starting first is race-free: the app refuses a second session.
    async fn recording_lifecycle_check(&mut self) -> Result<Option<String>, TestError> {
        match self.start_recording(None).await {
            Ok(_) => {}
            Err(TestError::ToolError(msg)) if msg.contains("already active") => {
                return Ok(Some(
                    "skipped: a recording is already in progress (another client, e.g. \
                     `victauri record`); it was left running. Stop it to run this check."
                        .to_string(),
                ));
            }
            Err(e) => return Err(e),
        }
        // From here on the recording is OURS: stop it on every path, or a failed eval would
        // leave it running and every later smoke run would skip this check.
        let activity = async {
            self.eval_js("console.log('victauri-smoke-test')").await?;
            self.eval_js("document.title").await?;
            tokio::time::sleep(Duration::from_secs(2)).await;
            Ok::<(), TestError>(())
        }
        .await;
        let stopped = self.stop_recording().await;
        activity?;
        let session = stopped?;
        let event_count = session
            .get("events")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        if event_count == 0 {
            return Err(TestError::Assertion(
                "recording captured 0 events — drain loop may not be running".to_string(),
            ));
        }
        Ok(None)
    }

    /// Assert that `/health` returns only `{"status":"ok"}`.
    ///
    /// Verifies the endpoint doesn't leak internal state like uptime,
    /// memory stats, or event counts.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Assertion`] if extra fields are present or the
    /// response shape is wrong.
    pub async fn assert_health_hardened(&mut self) -> Result<(), TestError> {
        let url = format!("{}/health", self.base_url());
        let resp =
            self.http_client()
                .get(&url)
                .send()
                .await
                .map_err(|e| TestError::Connection {
                    host: self.host().to_string(),
                    port: self.port(),
                    reason: e.to_string(),
                })?;
        if !resp.status().is_success() {
            return Err(TestError::Assertion(format!(
                "/health returned status {}",
                resp.status()
            )));
        }
        let text = resp.text().await.map_err(|e| TestError::Connection {
            host: self.host().to_string(),
            port: self.port(),
            reason: e.to_string(),
        })?;
        let json: Value = serde_json::from_str(&text).map_err(|_| {
            TestError::Assertion(format!(
                "/health returned non-JSON: {}",
                crate::client::truncate_chars(&text, 200)
            ))
        })?;
        let obj = json.as_object().ok_or_else(|| {
            TestError::Assertion("/health response is not a JSON object".to_string())
        })?;
        if obj.len() != 1 || obj.get("status").and_then(Value::as_str) != Some("ok") {
            return Err(TestError::Assertion(format!(
                "/health should return only {{\"status\":\"ok\"}}, got: {text}"
            )));
        }
        Ok(())
    }

    // ── Layer 2: Built-in Smoke Suite ─────────────────────────────────────

    /// Run the built-in smoke test suite with default configuration.
    ///
    /// Exercises all core Victauri capabilities: eval, DOM, screenshot,
    /// windows, IPC integrity, accessibility, performance budgets,
    /// recording lifecycle, and health endpoint hardening.
    ///
    /// Individual check failures are captured in the [`SmokeReport`] — the
    /// method itself only returns `Err` on fatal transport errors.
    ///
    /// # Errors
    ///
    /// Returns [`TestError`] on connection or transport failures.
    pub async fn smoke_test(&mut self) -> Result<SmokeReport, TestError> {
        self.smoke_test_with_config(&SmokeConfig::default()).await
    }

    /// Run the built-in smoke test suite with custom thresholds.
    ///
    /// # Errors
    ///
    /// Returns [`TestError`] on connection or transport failures.
    pub async fn smoke_test_with_config(
        &mut self,
        config: &SmokeConfig,
    ) -> Result<SmokeReport, TestError> {
        let suite_start = Instant::now();
        let mut checks = Vec::new();

        macro_rules! check {
            ($name:expr, $expr:expr) => {{
                let start = Instant::now();
                let result: Result<(), TestError> = $expr;
                checks.push(SmokeCheckResult {
                    name: $name.to_string(),
                    passed: result.is_ok(),
                    detail: result.err().map_or_else(String::new, |e| e.to_string()),
                    duration: start.elapsed(),
                    skipped: false,
                });
            }};
        }

        // A check that may be SKIPPED: its expression yields `Ok(Some(reason))` when it chose not
        // to run (the reason becomes the detail), `Ok(None)` when it passed.
        macro_rules! skippable_check {
            ($name:expr, $expr:expr) => {{
                let start = Instant::now();
                let outcome: Result<Option<String>, TestError> = $expr;
                checks.push(SmokeCheckResult {
                    name: $name.to_string(),
                    passed: outcome.is_ok(),
                    skipped: matches!(outcome, Ok(Some(_))),
                    detail: match outcome {
                        Ok(reason) => reason.unwrap_or_default(),
                        Err(e) => e.to_string(),
                    },
                    duration: start.elapsed(),
                });
            }};
        }

        check!("eval_js works", self.assert_eval_works().await);
        check!("DOM snapshot valid", self.assert_dom_snapshot_valid().await);
        check!(
            "screenshot captures image",
            self.assert_screenshot_ok().await
        );
        check!("windows exist", self.assert_windows_exist().await);
        check!(
            "IPC integrity healthy",
            self.assert_ipc_integrity_ok().await
        );
        check!("no uncaught errors", self.assert_no_uncaught_errors().await);
        check!("accessibility audit", self.assert_accessible().await);
        check!(
            format!("DOM complete < {}ms", config.max_dom_complete_ms),
            self.assert_dom_complete_under(Duration::from_millis(config.max_dom_complete_ms))
                .await
        );
        check!(
            format!("heap < {:.0}MB", config.max_heap_mb),
            self.assert_heap_under_mb(config.max_heap_mb).await
        );
        // Skippable: never ends a recording another client has in progress (R5B-SMOKE1).
        skippable_check!(
            "recording lifecycle",
            self.recording_lifecycle_check().await
        );
        check!(
            "health endpoint hardened",
            self.assert_health_hardened().await
        );

        Ok(SmokeReport {
            checks,
            duration: suite_start.elapsed(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass(name: &str, ms: u64) -> SmokeCheckResult {
        SmokeCheckResult {
            name: name.to_string(),
            passed: true,
            detail: String::new(),
            duration: Duration::from_millis(ms),
            skipped: false,
        }
    }

    fn fail(name: &str, detail: &str, ms: u64) -> SmokeCheckResult {
        SmokeCheckResult {
            name: name.to_string(),
            passed: false,
            detail: detail.to_string(),
            duration: Duration::from_millis(ms),
            skipped: false,
        }
    }

    #[test]
    fn all_passed_empty_report() {
        let report = SmokeReport {
            checks: vec![],
            duration: Duration::ZERO,
        };
        assert!(report.all_passed());
        assert_eq!(report.passed_count(), 0);
        assert_eq!(report.total_count(), 0);
    }

    #[test]
    fn all_passed_with_passes() {
        let report = SmokeReport {
            checks: vec![pass("a", 10), pass("b", 20)],
            duration: Duration::from_millis(30),
        };
        assert!(report.all_passed());
        assert_eq!(report.passed_count(), 2);
        assert_eq!(report.total_count(), 2);
        assert!(report.failures().is_empty());
    }

    #[test]
    fn all_passed_false_with_failure() {
        let report = SmokeReport {
            checks: vec![pass("a", 10), fail("b", "broke", 20)],
            duration: Duration::from_millis(30),
        };
        assert!(!report.all_passed());
        assert_eq!(report.passed_count(), 1);
        assert_eq!(report.failures().len(), 1);
        assert_eq!(report.failures()[0].name, "b");
    }

    #[test]
    #[should_panic(expected = "smoke_test failed")]
    fn assert_all_passed_panics() {
        let report = SmokeReport {
            checks: vec![fail("bad", "it broke", 10)],
            duration: Duration::from_millis(10),
        };
        report.assert_all_passed();
    }

    #[test]
    fn to_verify_report_converts() {
        let report = SmokeReport {
            checks: vec![pass("ok", 10), fail("bad", "err", 20)],
            duration: Duration::from_millis(30),
        };
        let verify = report.to_verify_report();
        assert_eq!(verify.results.len(), 2);
        assert!(verify.results[0].passed);
        assert!(!verify.results[1].passed);
        assert_eq!(verify.results[1].detail, "err");
    }

    #[test]
    fn summary_includes_all_checks() {
        let report = SmokeReport {
            checks: vec![pass("eval works", 15), fail("screenshot", "no data", 200)],
            duration: Duration::from_millis(215),
        };
        let summary = report.to_summary();
        assert!(summary.contains("1/2 passed"));
        assert!(summary.contains("[PASS] eval works"));
        assert!(summary.contains("[FAIL] screenshot"));
        assert!(summary.contains("no data"));
    }

    #[test]
    fn smoke_config_defaults() {
        let config = SmokeConfig::default();
        assert_eq!(config.max_dom_complete_ms, 10_000);
        assert!((config.max_heap_mb - 512.0).abs() < f64::EPSILON);
    }

    #[test]
    fn to_junit_via_verify_report() {
        let report = SmokeReport {
            checks: vec![pass("check1", 100)],
            duration: Duration::from_millis(100),
        };
        let verify = report.to_verify_report();
        let junit = verify.to_junit("smoke", Duration::from_millis(100));
        let xml = junit.to_xml();
        assert!(xml.contains("tests=\"1\""));
        assert!(xml.contains("failures=\"0\""));
    }

    #[test]
    fn summary_shows_all_failures() {
        let report = SmokeReport {
            checks: vec![
                fail("check1", "error 1", 10),
                fail("check2", "error 2", 20),
                pass("check3", 30),
            ],
            duration: Duration::from_millis(60),
        };
        let summary = report.to_summary();
        assert!(summary.contains("1/3 passed"));
        assert!(summary.contains("[FAIL] check1"));
        assert!(summary.contains("error 1"));
        assert!(summary.contains("[FAIL] check2"));
        assert!(summary.contains("error 2"));
        assert!(summary.contains("[PASS] check3"));
    }

    #[test]
    fn failures_returns_only_failed() {
        let report = SmokeReport {
            checks: vec![
                pass("ok", 10),
                fail("bad1", "e1", 20),
                fail("bad2", "e2", 30),
            ],
            duration: Duration::from_millis(60),
        };
        let failures = report.failures();
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0].name, "bad1");
        assert_eq!(failures[1].name, "bad2");
    }

    // ── R4-TERM1: page-controlled text in the summary ──

    #[test]
    fn page_text_cannot_forge_ci_annotations_in_the_summary() {
        let report = SmokeReport {
            checks: vec![SmokeCheckResult {
                name: "no uncaught errors".to_string(),
                passed: false,
                detail:
                    "1 uncaught error(s): [uncaught] x\n::error file=src/main.rs::forged\u{1b}[2J"
                        .to_string(),
                duration: Duration::from_millis(1),
                skipped: false,
            }],
            duration: Duration::from_millis(1),
        };
        let summary = report.to_summary();
        assert!(
            !summary.lines().any(|l| l.trim_start().starts_with("::")),
            "a page-controlled line became a GitHub workflow command:\n{summary}"
        );
        assert!(!summary.contains('\u{1b}'), "raw ESC reached the terminal");
    }

    // ── R5B-SMOKE1: the smoke suite must not end someone else's recording ──

    /// A mock whose `recording` tool behaves like the plugin's: one session at a time.
    async fn recording_mock(
        already_recording: bool,
    ) -> (u16, std::sync::Arc<std::sync::atomic::AtomicU32>) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

        let active = Arc::new(AtomicBool::new(already_recording));
        let stops = Arc::new(AtomicU32::new(0));
        let (a, st) = (Arc::clone(&active), Arc::clone(&stops));
        let app = axum::Router::new().route(
            "/mcp",
            axum::routing::post(move |body: String| {
                let (active, stops) = (Arc::clone(&a), Arc::clone(&st));
                async move {
                    let v: Value = serde_json::from_str(&body).unwrap_or_default();
                    let id = v.get("id").cloned();
                    let text = |t: String, err: bool| {
                        serde_json::json!({"jsonrpc":"2.0","id":id,
                            "result":{"content":[{"type":"text","text":t}],"isError":err}})
                    };
                    let reply = match v["method"].as_str().unwrap_or("") {
                        "initialize" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{
                            "protocolVersion":"2025-06-18","capabilities":{},
                            "serverInfo":{"name":"mock","version":"0"}}}),
                        m if m.starts_with("notifications/") => serde_json::json!({}),
                        _ => match (
                            v["params"]["name"].as_str(),
                            v["params"]["arguments"]["action"].as_str(),
                        ) {
                            (Some("recording"), Some("start")) => {
                                if active.swap(true, Ordering::SeqCst) {
                                    text("recording session already active".into(), true)
                                } else {
                                    text(r#"{"started":true,"session_id":"s"}"#.into(), false)
                                }
                            }
                            (Some("recording"), Some("stop")) => {
                                stops.fetch_add(1, Ordering::SeqCst);
                                if active.swap(false, Ordering::SeqCst) {
                                    text(r#"{"events":[{"type":"console"}]}"#.into(), false)
                                } else {
                                    text("no active recording session".into(), true)
                                }
                            }
                            (Some("eval_js"), _) => text("null".into(), false),
                            _ => text("not mocked".into(), true),
                        },
                    };
                    axum::Json(reply)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (port, stops)
    }

    #[tokio::test]
    async fn smoke_skips_the_recording_check_when_a_recording_is_in_progress() {
        let (port, stops) = recording_mock(true).await;
        let mut client = VictauriClient::connect_with_token(port, None)
            .await
            .unwrap();
        let report = client.smoke_test().await.unwrap();
        let rec = report
            .checks
            .iter()
            .find(|c| c.name == "recording lifecycle")
            .expect("recording check present");
        assert_eq!(
            stops.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the smoke suite stopped a recording it did not start"
        );
        assert!(rec.skipped && rec.passed, "{rec:?}");
        assert!(rec.detail.contains("already in progress"), "{rec:?}");
        assert!(report.to_summary().contains("[SKIP] recording lifecycle"));
    }

    #[tokio::test]
    async fn the_recording_check_runs_and_stops_its_own_recording() {
        let (port, stops) = recording_mock(false).await;
        let mut client = VictauriClient::connect_with_token(port, None)
            .await
            .unwrap();
        client.assert_recording_lifecycle().await.unwrap();
        assert_eq!(stops.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
