//! IPC coverage tracking — measures which Tauri commands are exercised by tests.
//!
//! Compares the set of registered commands (from the registry) against the set
//! of IPC calls observed during a test session. Reports tested vs. untested
//! commands, call counts, and coverage percentage.

use serde_json::Value;

use crate::client::VictauriClient;
use crate::error::TestError;

/// IPC coverage report showing which commands were exercised.
#[derive(Debug)]
pub struct CoverageReport {
    /// Total number of registered commands.
    pub total_commands: usize,
    /// Number of commands invoked at least once.
    pub tested_commands: usize,
    /// Coverage percentage (0.0 to 100.0).
    pub coverage_percentage: f64,
    /// Commands that were never invoked during the session.
    pub untested: Vec<String>,
    /// Commands sorted by invocation count (descending).
    pub most_called: Vec<CommandCalls>,
}

/// A command name with its invocation count.
#[derive(Debug)]
pub struct CommandCalls {
    /// Name of the Tauri command.
    pub name: String,
    /// Number of times invoked during the session.
    pub calls: usize,
}

impl CoverageReport {
    /// Returns true if coverage meets or exceeds the given threshold.
    #[must_use]
    pub fn meets_threshold(&self, threshold_percent: f64) -> bool {
        self.coverage_percentage >= threshold_percent
    }

    /// Formats the report for human-readable output.
    #[must_use]
    pub fn to_summary(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str(&format!(
            "IPC Coverage: {:.1}% ({}/{} commands tested)\n",
            self.coverage_percentage, self.tested_commands, self.total_commands
        ));

        if !self.most_called.is_empty() {
            out.push_str("\nMost called:\n");
            for cmd in self.most_called.iter().take(10) {
                out.push_str(&format!("  {:>4}x  {}\n", cmd.calls, cmd.name));
            }
        }

        if !self.untested.is_empty() {
            out.push_str(&format!("\nUntested ({}):\n", self.untested.len()));
            for name in self.untested.iter().take(20) {
                out.push_str(&format!("  - {name}\n"));
            }
            if self.untested.len() > 20 {
                out.push_str(&format!("  ... and {} more\n", self.untested.len() - 20));
            }
        }

        out
    }
}

/// Builds a coverage report by comparing the command registry against IPC call logs.
///
/// Queries the running app for its command registry and IPC call history,
/// then computes which commands were exercised and which remain untested.
///
/// # Errors
///
/// Returns errors from the underlying MCP tool calls.
pub async fn coverage_report(client: &mut VictauriClient) -> Result<CoverageReport, TestError> {
    let registry = client.get_registry().await?;
    let ipc_log = client.get_ipc_log(None).await?;

    let registered: Vec<String> = extract_command_names(&registry);
    let called: Vec<String> = extract_ipc_commands(&ipc_log);

    build_report(&registered, &called)
}

/// Asserts that IPC coverage meets the given threshold, panicking with a
/// detailed report if it does not.
///
/// # Errors
///
/// Returns errors from the underlying MCP tool calls.
///
/// # Panics
///
/// Panics if coverage falls below `threshold_percent`.
pub async fn assert_coverage_above(
    client: &mut VictauriClient,
    threshold_percent: f64,
) -> Result<(), TestError> {
    let report = coverage_report(client).await?;
    assert!(
        report.meets_threshold(threshold_percent),
        "IPC coverage {:.1}% is below threshold {:.1}%\n{}",
        report.coverage_percentage,
        threshold_percent,
        report.to_summary()
    );
    Ok(())
}

fn extract_command_names(registry: &Value) -> Vec<String> {
    if let Some(arr) = registry.as_array() {
        arr.iter()
            .filter_map(|v| v.get("name").and_then(Value::as_str).map(String::from))
            .collect()
    } else if let Some(commands) = registry.get("commands").and_then(Value::as_array) {
        commands
            .iter()
            .filter_map(|v| v.get("name").and_then(Value::as_str).map(String::from))
            .collect()
    } else {
        Vec::new()
    }
}

fn extract_ipc_commands(ipc_log: &Value) -> Vec<String> {
    if let Some(arr) = ipc_log.as_array() {
        arr.iter()
            .filter_map(|v| v.get("command").and_then(Value::as_str).map(String::from))
            .collect()
    } else {
        Vec::new()
    }
}

fn build_report(registered: &[String], called: &[String]) -> Result<CoverageReport, TestError> {
    let mut call_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for cmd in called {
        let name = cmd
            .strip_prefix("plugin:")
            .and_then(|s| s.split('|').nth(1))
            .unwrap_or(cmd);
        *call_counts.entry(name).or_default() += 1;
    }

    let total_commands = registered.len();
    let mut tested = 0;
    let mut untested = Vec::new();
    let mut most_called: Vec<CommandCalls> = Vec::new();

    for name in registered {
        let clean = name
            .strip_prefix("plugin:")
            .and_then(|s| s.split('|').nth(1))
            .unwrap_or(name);
        if let Some(&count) = call_counts.get(clean) {
            tested += 1;
            most_called.push(CommandCalls {
                name: clean.to_string(),
                calls: count,
            });
        } else {
            untested.push(clean.to_string());
        }
    }

    most_called.sort_by_key(|c| std::cmp::Reverse(c.calls));
    untested.sort();

    // An empty registry has NOTHING to cover, so report 0% rather than a
    // misleading 100% "fully covered" — reading a no-commands run as success is a
    // false positive (red-team P3). The CLI surfaces this as an explicit
    // "no commands registered" warning (and exits non-zero unless
    // `--allow-empty-registry`); callers of the library can check
    // `total_commands == 0` to distinguish "unmeasurable" from "0% covered".
    let coverage_percentage = if total_commands == 0 {
        0.0
    } else {
        (tested as f64 / total_commands as f64) * 100.0
    };

    Ok(CoverageReport {
        total_commands,
        tested_commands: tested,
        coverage_percentage,
        untested,
        most_called,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_report_full_coverage() {
        let registered = vec!["cmd_a".to_string(), "cmd_b".to_string()];
        let called = vec![
            "cmd_a".to_string(),
            "cmd_b".to_string(),
            "cmd_a".to_string(),
        ];

        let report = build_report(&registered, &called).unwrap();
        assert_eq!(report.total_commands, 2);
        assert_eq!(report.tested_commands, 2);
        assert_eq!(report.coverage_percentage, 100.0);
        assert!(report.untested.is_empty());
        assert_eq!(report.most_called[0].name, "cmd_a");
        assert_eq!(report.most_called[0].calls, 2);
    }

    #[test]
    fn build_report_partial_coverage() {
        let registered = vec![
            "cmd_a".to_string(),
            "cmd_b".to_string(),
            "cmd_c".to_string(),
        ];
        let called = vec!["cmd_a".to_string()];

        let report = build_report(&registered, &called).unwrap();
        assert_eq!(report.tested_commands, 1);
        assert!((report.coverage_percentage - 33.333).abs() < 0.01);
        assert_eq!(report.untested.len(), 2);
        assert!(report.untested.contains(&"cmd_b".to_string()));
        assert!(report.untested.contains(&"cmd_c".to_string()));
    }

    #[test]
    fn build_report_no_commands() {
        // No registered commands = nothing measurable = 0%, NOT a false 100%.
        let report = build_report(&[], &[]).unwrap();
        assert_eq!(report.coverage_percentage, 0.0);
        assert_eq!(report.total_commands, 0);
    }

    #[test]
    fn build_report_strips_plugin_prefix() {
        let registered = vec!["save_data".to_string()];
        let called = vec!["plugin:myapp|save_data".to_string()];

        let report = build_report(&registered, &called).unwrap();
        assert_eq!(report.tested_commands, 1);
        assert_eq!(report.coverage_percentage, 100.0);
    }

    #[test]
    fn meets_threshold_boundary() {
        let report = CoverageReport {
            total_commands: 10,
            tested_commands: 8,
            coverage_percentage: 80.0,
            untested: vec!["a".to_string(), "b".to_string()],
            most_called: vec![],
        };
        assert!(report.meets_threshold(80.0));
        assert!(!report.meets_threshold(80.1));
    }

    #[test]
    fn summary_formatting() {
        let report = CoverageReport {
            total_commands: 3,
            tested_commands: 1,
            coverage_percentage: 33.3,
            untested: vec!["cmd_b".to_string(), "cmd_c".to_string()],
            most_called: vec![CommandCalls {
                name: "cmd_a".to_string(),
                calls: 5,
            }],
        };
        let summary = report.to_summary();

        assert!(summary.contains("33.3%"));
        assert!(summary.contains("1/3"));
        assert!(summary.contains("cmd_a"));
        assert!(summary.contains("cmd_b"));
        assert!(summary.contains("Untested (2)"));
    }

    #[test]
    fn extract_command_names_from_array() {
        let registry = serde_json::json!([
            {"name": "cmd_a", "description": "A"},
            {"name": "cmd_b", "description": "B"}
        ]);
        let names = extract_command_names(&registry);
        assert_eq!(names, vec!["cmd_a", "cmd_b"]);
    }

    #[test]
    fn extract_command_names_from_commands_field() {
        let registry = serde_json::json!({
            "commands": [
                {"name": "cmd_x"},
                {"name": "cmd_y"}
            ]
        });
        let names = extract_command_names(&registry);
        assert_eq!(names, vec!["cmd_x", "cmd_y"]);
    }

    #[test]
    fn extract_ipc_commands_from_log() {
        let log = serde_json::json!([
            {"command": "greet", "status": "ok"},
            {"command": "save", "status": "ok"}
        ]);
        let cmds = extract_ipc_commands(&log);
        assert_eq!(cmds, vec!["greet", "save"]);
    }

    #[test]
    fn meets_threshold_exact_boundary() {
        let report = build_report(&["a".to_string(), "b".to_string()], &["a".to_string()]).unwrap();
        // 1 out of 2 = 50.0%
        assert!(report.meets_threshold(50.0));
        assert!(!report.meets_threshold(50.1));
    }

    #[test]
    fn summary_includes_all_sections() {
        let report = build_report(
            &[
                "cmd_a".to_string(),
                "cmd_b".to_string(),
                "cmd_c".to_string(),
            ],
            &["cmd_a".to_string(), "cmd_a".to_string()],
        )
        .unwrap();
        let summary = report.to_summary();
        assert!(summary.contains("IPC Coverage:"));
        assert!(summary.contains("Most called:"));
        assert!(summary.contains("Untested"));
        assert!(summary.contains("cmd_a"));
        assert!(summary.contains("cmd_b"));
        assert!(summary.contains("cmd_c"));
    }

    #[test]
    fn extract_command_names_empty_object() {
        let registry = serde_json::json!({});
        let names = extract_command_names(&registry);
        assert!(names.is_empty());
    }

    #[test]
    fn extract_command_names_null_input() {
        let registry = serde_json::json!(null);
        let names = extract_command_names(&registry);
        assert!(names.is_empty());
    }

    #[test]
    fn extract_ipc_commands_empty_array() {
        let log = serde_json::json!([]);
        let cmds = extract_ipc_commands(&log);
        assert!(cmds.is_empty());
    }

    #[test]
    fn extract_ipc_commands_non_array() {
        let log = serde_json::json!("not an array");
        let cmds = extract_ipc_commands(&log);
        assert!(cmds.is_empty());
    }
}
