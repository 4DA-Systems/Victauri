use std::collections::HashSet;

use crate::redaction::Redactor;

/// Privacy profile controlling which MCP tools and actions are permitted.
///
/// The three tiers form a strict hierarchy: `Observe ⊂ Test ⊂ FullControl`.
/// Each higher tier inherits all permissions from the tier below and adds more.
///
/// | Profile | Can read | Can interact | Can mutate | Can eval/screenshot |
/// |---|---|---|---|---|
/// | `Observe` | Yes | No | No | No |
/// | `Test` | Yes | Yes | Storage writes | No |
/// | `FullControl` | Yes | Yes | Yes | Yes |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrivacyProfile {
    /// Read-only observation. Snapshots, logs, registry, accessibility, performance,
    /// window state — but no clicks, no input, no eval, no screenshots, no mutations.
    Observe,
    /// Observation + UI interactions + storage writes + recording. Suitable for
    /// automated testing. Eval, screenshot, CSS injection, navigation, and
    /// `invoke_command` (unless allowlisted) remain blocked.
    Test,
    /// Everything permitted. No restrictions. This is the default.
    #[default]
    FullControl,
}

impl std::fmt::Display for PrivacyProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Observe => write!(f, "observe"),
            Self::Test => write!(f, "test"),
            Self::FullControl => write!(f, "full_control"),
        }
    }
}

/// Privacy controls for the MCP server.
///
/// Combines a [`PrivacyProfile`] (tiered permission matrix) with fine-grained
/// overrides: command allowlists/blocklists, per-tool disabling, and output redaction.
///
/// **Precedence:** explicit `disabled_tools` overrides → profile matrix → allowlist/blocklist.
#[derive(Default)]
pub struct PrivacyConfig {
    /// The active privacy profile tier.
    pub profile: PrivacyProfile,
    /// If set, only these Tauri commands can be invoked (positive allowlist).
    pub command_allowlist: Option<HashSet<String>>,
    /// Tauri commands that are always blocked, even if on the allowlist.
    pub command_blocklist: HashSet<String>,
    /// MCP tool/action names explicitly disabled (override layer on top of profile).
    pub disabled_tools: HashSet<String>,
    /// Output redactor with regex and JSON-key matching.
    pub redactor: Redactor,
    /// Whether output redaction is active.
    pub redaction_enabled: bool,
}

impl PrivacyConfig {
    /// Returns `true` if the Tauri command passes both the allowlist and blocklist.
    #[must_use]
    pub fn is_command_allowed(&self, command: &str) -> bool {
        if self.command_blocklist.contains(command) {
            return false;
        }
        match &self.command_allowlist {
            Some(allow) => allow.contains(command),
            None => true,
        }
    }

    /// Returns `true` if the given tool or qualified action (e.g. `"window.manage"`)
    /// is permitted by the current profile AND not in the explicit disabled set.
    #[must_use]
    pub fn is_tool_enabled(&self, tool_or_action: &str) -> bool {
        if self.disabled_tools.contains(tool_or_action) {
            return false;
        }
        is_allowed_by_profile(self.profile, tool_or_action)
    }

    /// Check whether `invoke_command` is allowed for a specific command name.
    ///
    /// In `Test` profile, `invoke_command` is only allowed if the command is on the
    /// allowlist. In `FullControl`, it's always allowed. In `Observe`, always blocked.
    #[must_use]
    pub fn is_invoke_allowed(&self, command: &str) -> bool {
        if self.disabled_tools.contains("invoke_command") {
            return false;
        }
        match self.profile {
            PrivacyProfile::FullControl => true,
            PrivacyProfile::Test => self
                .command_allowlist
                .as_ref()
                .is_some_and(|al| al.contains(command)),
            PrivacyProfile::Observe => false,
        }
    }

    /// Apply redaction rules to the output string if redaction is enabled.
    #[must_use]
    pub fn redact_output(&self, output: &str) -> String {
        if self.redaction_enabled {
            self.redactor.redact(output)
        } else {
            output.to_string()
        }
    }
}

/// The permission matrix. Maps `(profile, tool_or_action)` → allowed.
///
/// Naming convention: standalone tools use bare names (`"eval_js"`), compound tool
/// actions use dot-qualified names (`"window.manage"`, `"input.fill"`).
///
/// Everything not explicitly listed defaults to allowed (open-world for new tools).
#[must_use]
fn is_allowed_by_profile(profile: PrivacyProfile, tool_or_action: &str) -> bool {
    match profile {
        PrivacyProfile::FullControl => true,
        PrivacyProfile::Test => matches!(
            tool_or_action,
            // Read-only observation (superset of Observe)
            "dom_snapshot"
                | "find_elements"
                | "get_registry"
                | "get_memory_stats"
                | "get_plugin_info"
                | "get_diagnostics"
                | "detect_ghost_commands"
                | "check_ipc_integrity"
                | "resolve_command"
                | "wait_for"
                // Assertions (use eval internally but are test-oriented)
                | "verify_state"
                | "assert_semantic"
                // Interactions
                | "interact"
                | "interact.click"
                | "interact.double_click"
                | "interact.hover"
                | "interact.focus"
                | "interact.scroll_into_view"
                | "interact.select_option"
                // Input
                | "fill"
                | "input"
                | "input.fill"
                | "type_text"
                | "input.type_text"
                | "input.press_key"
                // Storage (read + write)
                | "storage"
                | "set_storage"
                | "storage.set"
                | "delete_storage"
                | "storage.delete"
                | "storage.get"
                | "storage.get_cookies"
                | "get_storage"
                | "get_cookies"
                // Recording
                | "recording"
                | "recording.start"
                | "recording.stop"
                | "recording.checkpoint"
                | "recording.list_checkpoints"
                | "recording.get_events"
                | "recording.events_between"
                | "recording.get_replay"
                | "recording.export"
                | "recording.import"
                // Logs (all read-only)
                | "logs"
                | "logs.console"
                | "logs.network"
                | "logs.ipc"
                | "logs.navigation"
                | "logs.dialogs"
                | "logs.events"
                | "logs.slow_ipc"
                // Inspect (read-only + visual debug)
                | "inspect"
                | "inspect.styles"
                | "inspect.bounds"
                | "inspect.highlight"
                | "inspect.clear_highlights"
                | "inspect.audit_a11y"
                | "inspect.performance"
                // Window (read-only)
                | "list_windows"
                | "window"
                | "window.get_state"
                | "window.list"
                | "get_window_state"
                // Navigate (read-only actions)
                | "navigate.go_back"
                | "navigate.get_history"
                | "navigate.get_dialog_log"
        ),
        PrivacyProfile::Observe => matches!(
            tool_or_action,
            "dom_snapshot"
                | "find_elements"
                | "get_registry"
                | "get_memory_stats"
                | "get_plugin_info"
                | "get_diagnostics"
                | "detect_ghost_commands"
                | "check_ipc_integrity"
                | "resolve_command"
                | "logs"
                | "logs.console"
                | "logs.network"
                | "logs.ipc"
                | "logs.navigation"
                | "logs.dialogs"
                | "logs.events"
                | "logs.slow_ipc"
                | "inspect"
                | "inspect.styles"
                | "inspect.bounds"
                | "inspect.highlight"
                | "inspect.clear_highlights"
                | "inspect.audit_a11y"
                | "inspect.performance"
                | "list_windows"
                | "window"
                | "window.get_state"
                | "window.list"
                | "get_window_state"
                | "wait_for"
        ),
    }
}

/// Create a [`PrivacyConfig`] for the `Observe` profile with redaction enabled.
#[must_use]
pub fn observe_privacy_config() -> PrivacyConfig {
    PrivacyConfig {
        profile: PrivacyProfile::Observe,
        command_allowlist: None,
        command_blocklist: HashSet::new(),
        disabled_tools: HashSet::new(),
        redactor: Redactor::default(),
        redaction_enabled: true,
    }
}

/// Create a [`PrivacyConfig`] for the `Test` profile with redaction enabled.
#[must_use]
pub fn test_privacy_config() -> PrivacyConfig {
    PrivacyConfig {
        profile: PrivacyProfile::Test,
        command_allowlist: None,
        command_blocklist: HashSet::new(),
        disabled_tools: HashSet::new(),
        redactor: Redactor::default(),
        redaction_enabled: true,
    }
}

/// Create a [`PrivacyConfig`] that disables dangerous tools and enables redaction.
///
/// This is an alias for [`observe_privacy_config()`] — strict mode maps to the
/// `Observe` profile.
#[must_use]
pub fn strict_privacy_config() -> PrivacyConfig {
    observe_privacy_config()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Command filtering ──────────────────────────────────────────────────

    #[test]
    fn default_allows_all_commands() {
        let config = PrivacyConfig::default();
        assert!(config.is_command_allowed("get_settings"));
        assert!(config.is_command_allowed("anything"));
    }

    #[test]
    fn blocklist_blocks() {
        let mut config = PrivacyConfig::default();
        config.command_blocklist.insert("save_api_key".to_string());
        assert!(!config.is_command_allowed("save_api_key"));
        assert!(config.is_command_allowed("get_settings"));
    }

    #[test]
    fn allowlist_restricts() {
        let mut allow = HashSet::new();
        allow.insert("get_settings".to_string());
        allow.insert("get_monitoring_status".to_string());
        let config = PrivacyConfig {
            command_allowlist: Some(allow),
            ..Default::default()
        };
        assert!(config.is_command_allowed("get_settings"));
        assert!(!config.is_command_allowed("save_api_key"));
    }

    #[test]
    fn blocklist_wins_over_allowlist() {
        let mut allow = HashSet::new();
        allow.insert("save_api_key".to_string());
        let mut block = HashSet::new();
        block.insert("save_api_key".to_string());
        let config = PrivacyConfig {
            command_allowlist: Some(allow),
            command_blocklist: block,
            ..Default::default()
        };
        assert!(!config.is_command_allowed("save_api_key"));
    }

    // ── Profile: FullControl ───────────────────────────────────────────────

    #[test]
    fn full_control_allows_everything() {
        let config = PrivacyConfig::default();
        assert_eq!(config.profile, PrivacyProfile::FullControl);
        assert!(config.is_tool_enabled("eval_js"));
        assert!(config.is_tool_enabled("screenshot"));
        assert!(config.is_tool_enabled("invoke_command"));
        assert!(config.is_tool_enabled("interact"));
        assert!(config.is_tool_enabled("interact.click"));
        assert!(config.is_tool_enabled("input.fill"));
        assert!(config.is_tool_enabled("window.manage"));
        assert!(config.is_tool_enabled("navigate"));
        assert!(config.is_tool_enabled("navigate.go_to"));
        assert!(config.is_tool_enabled("css.inject"));
        assert!(config.is_tool_enabled("recording"));
        assert!(config.is_tool_enabled("storage.set"));
        assert!(config.is_tool_enabled("set_dialog_response"));
    }

    // ── Profile: Test ──────────────────────────────────────────────────────

    #[test]
    fn test_profile_allows_interactions() {
        let config = test_privacy_config();
        assert!(config.is_tool_enabled("interact"));
        assert!(config.is_tool_enabled("interact.click"));
        assert!(config.is_tool_enabled("interact.double_click"));
        assert!(config.is_tool_enabled("interact.hover"));
        assert!(config.is_tool_enabled("interact.focus"));
        assert!(config.is_tool_enabled("interact.scroll_into_view"));
        assert!(config.is_tool_enabled("interact.select_option"));
    }

    #[test]
    fn test_profile_allows_input() {
        let config = test_privacy_config();
        assert!(config.is_tool_enabled("fill"));
        assert!(config.is_tool_enabled("input.fill"));
        assert!(config.is_tool_enabled("type_text"));
        assert!(config.is_tool_enabled("input.type_text"));
        assert!(config.is_tool_enabled("input.press_key"));
    }

    #[test]
    fn test_profile_allows_storage_writes() {
        let config = test_privacy_config();
        assert!(config.is_tool_enabled("set_storage"));
        assert!(config.is_tool_enabled("storage.set"));
        assert!(config.is_tool_enabled("delete_storage"));
        assert!(config.is_tool_enabled("storage.delete"));
    }

    #[test]
    fn test_profile_allows_recording() {
        let config = test_privacy_config();
        assert!(config.is_tool_enabled("recording"));
        assert!(config.is_tool_enabled("recording.start"));
        assert!(config.is_tool_enabled("recording.stop"));
    }

    #[test]
    fn test_profile_blocks_eval_and_screenshot() {
        let config = test_privacy_config();
        assert!(!config.is_tool_enabled("eval_js"));
        assert!(!config.is_tool_enabled("screenshot"));
    }

    #[test]
    fn test_profile_blocks_navigation() {
        let config = test_privacy_config();
        assert!(!config.is_tool_enabled("navigate"));
        assert!(!config.is_tool_enabled("navigate.go_to"));
        assert!(!config.is_tool_enabled("set_dialog_response"));
        assert!(!config.is_tool_enabled("navigate.set_dialog_response"));
    }

    #[test]
    fn test_profile_blocks_window_mutations() {
        let config = test_privacy_config();
        assert!(!config.is_tool_enabled("window.manage"));
        assert!(!config.is_tool_enabled("window.resize"));
        assert!(!config.is_tool_enabled("window.move_to"));
        assert!(!config.is_tool_enabled("window.set_title"));
    }

    #[test]
    fn test_profile_blocks_css_injection() {
        let config = test_privacy_config();
        assert!(!config.is_tool_enabled("inject_css"));
        assert!(!config.is_tool_enabled("css.inject"));
        assert!(!config.is_tool_enabled("css.remove"));
    }

    #[test]
    fn test_profile_blocks_invoke_command() {
        let config = test_privacy_config();
        assert!(!config.is_tool_enabled("invoke_command"));
    }

    #[test]
    fn test_profile_allows_read_only_tools() {
        let config = test_privacy_config();
        assert!(config.is_tool_enabled("dom_snapshot"));
        assert!(config.is_tool_enabled("find_elements"));
        assert!(config.is_tool_enabled("verify_state"));
        assert!(config.is_tool_enabled("detect_ghost_commands"));
        assert!(config.is_tool_enabled("check_ipc_integrity"));
        assert!(config.is_tool_enabled("get_registry"));
        assert!(config.is_tool_enabled("get_memory_stats"));
        assert!(config.is_tool_enabled("get_plugin_info"));
        assert!(config.is_tool_enabled("resolve_command"));
        assert!(config.is_tool_enabled("wait_for"));
        assert!(config.is_tool_enabled("assert_semantic"));
    }

    // ── Profile: Observe ───────────────────────────────────────────────────

    #[test]
    fn observe_blocks_all_interactions() {
        let config = observe_privacy_config();
        assert!(!config.is_tool_enabled("interact"));
        assert!(!config.is_tool_enabled("interact.click"));
        assert!(!config.is_tool_enabled("interact.double_click"));
        assert!(!config.is_tool_enabled("interact.hover"));
        assert!(!config.is_tool_enabled("interact.focus"));
        assert!(!config.is_tool_enabled("interact.scroll_into_view"));
        assert!(!config.is_tool_enabled("interact.select_option"));
    }

    #[test]
    fn observe_blocks_all_input() {
        let config = observe_privacy_config();
        assert!(!config.is_tool_enabled("fill"));
        assert!(!config.is_tool_enabled("input.fill"));
        assert!(!config.is_tool_enabled("type_text"));
        assert!(!config.is_tool_enabled("input.type_text"));
        assert!(!config.is_tool_enabled("input.press_key"));
    }

    #[test]
    fn observe_blocks_storage_writes() {
        let config = observe_privacy_config();
        assert!(!config.is_tool_enabled("set_storage"));
        assert!(!config.is_tool_enabled("storage.set"));
        assert!(!config.is_tool_enabled("delete_storage"));
        assert!(!config.is_tool_enabled("storage.delete"));
    }

    #[test]
    fn observe_blocks_recording() {
        let config = observe_privacy_config();
        assert!(!config.is_tool_enabled("recording"));
        assert!(!config.is_tool_enabled("recording.start"));
        assert!(!config.is_tool_enabled("recording.stop"));
    }

    #[test]
    fn observe_blocks_dangerous_tools() {
        let config = observe_privacy_config();
        assert!(!config.is_tool_enabled("eval_js"));
        assert!(!config.is_tool_enabled("screenshot"));
        assert!(!config.is_tool_enabled("invoke_command"));
        assert!(!config.is_tool_enabled("navigate"));
        assert!(!config.is_tool_enabled("navigate.go_to"));
        assert!(!config.is_tool_enabled("inject_css"));
        assert!(!config.is_tool_enabled("css.inject"));
        assert!(!config.is_tool_enabled("css.remove"));
        assert!(!config.is_tool_enabled("window.manage"));
        assert!(!config.is_tool_enabled("window.resize"));
        assert!(!config.is_tool_enabled("window.move_to"));
        assert!(!config.is_tool_enabled("window.set_title"));
    }

    #[test]
    fn observe_allows_read_only_tools() {
        let config = observe_privacy_config();
        assert!(config.is_tool_enabled("dom_snapshot"));
        assert!(config.is_tool_enabled("find_elements"));
        assert!(config.is_tool_enabled("detect_ghost_commands"));
        assert!(config.is_tool_enabled("check_ipc_integrity"));
        assert!(config.is_tool_enabled("get_registry"));
        assert!(config.is_tool_enabled("get_memory_stats"));
        assert!(config.is_tool_enabled("get_plugin_info"));
        assert!(config.is_tool_enabled("get_diagnostics"));
        assert!(config.is_tool_enabled("resolve_command"));
        assert!(config.is_tool_enabled("wait_for"));
        assert!(config.is_tool_enabled("window")); // the compound tool itself (get_state, list)
    }

    #[test]
    fn observe_blocks_eval_dependent_tools() {
        let config = observe_privacy_config();
        // verify_state and assert_semantic depend on eval_js internally,
        // so they are excluded from the Observe allowlist.
        assert!(!config.is_tool_enabled("verify_state"));
        assert!(!config.is_tool_enabled("assert_semantic"));
    }

    #[test]
    fn observe_allows_read_actions_on_compound_tools() {
        let config = observe_privacy_config();
        // Window read actions
        assert!(config.is_tool_enabled("window.get_state"));
        assert!(config.is_tool_enabled("window.list"));
        // Logs (all read-only)
        assert!(config.is_tool_enabled("logs"));
        assert!(config.is_tool_enabled("logs.console"));
        assert!(config.is_tool_enabled("logs.network"));
        assert!(config.is_tool_enabled("logs.ipc"));
        assert!(config.is_tool_enabled("logs.navigation"));
        assert!(config.is_tool_enabled("logs.dialogs"));
        assert!(config.is_tool_enabled("logs.events"));
        assert!(config.is_tool_enabled("logs.slow_ipc"));
        // Inspect (read-only)
        assert!(config.is_tool_enabled("inspect"));
        assert!(config.is_tool_enabled("inspect.styles"));
        assert!(config.is_tool_enabled("inspect.bounds"));
        assert!(config.is_tool_enabled("inspect.highlight"));
        assert!(config.is_tool_enabled("inspect.clear_highlights"));
        assert!(config.is_tool_enabled("inspect.audit_a11y"));
        assert!(config.is_tool_enabled("inspect.performance"));
    }

    #[test]
    fn observe_blocks_unlisted_tools() {
        let config = observe_privacy_config();
        // Tools not in the closed-world allowlist are blocked
        assert!(!config.is_tool_enabled("navigate.get_history"));
        assert!(!config.is_tool_enabled("navigate.get_dialog_log"));
        assert!(!config.is_tool_enabled("css.get_styles"));
        assert!(!config.is_tool_enabled("css.get_computed"));
        assert!(!config.is_tool_enabled("storage.get"));
        assert!(!config.is_tool_enabled("storage.get_cookies"));
        assert!(!config.is_tool_enabled("navigate.go_back"));
    }

    #[test]
    fn observe_enables_redaction() {
        let config = observe_privacy_config();
        assert!(config.redaction_enabled);
    }

    // ── Explicit disable overrides profile ─────────────────────────────────

    #[test]
    fn disabled_tools_override_full_control() {
        let mut disabled = HashSet::new();
        disabled.insert("eval_js".to_string());
        let config = PrivacyConfig {
            profile: PrivacyProfile::FullControl,
            disabled_tools: disabled,
            ..Default::default()
        };
        assert!(!config.is_tool_enabled("eval_js"));
        assert!(config.is_tool_enabled("screenshot"));
    }

    #[test]
    fn disabled_tools_stack_with_profile() {
        let mut disabled = HashSet::new();
        disabled.insert("dom_snapshot".to_string());
        let mut config = test_privacy_config();
        config.disabled_tools = disabled;
        // Profile allows dom_snapshot, but explicit disable overrides
        assert!(!config.is_tool_enabled("dom_snapshot"));
        // Profile blocks eval_js
        assert!(!config.is_tool_enabled("eval_js"));
    }

    // ── invoke_command special handling ─────────────────────────────────────

    #[test]
    fn invoke_allowed_in_full_control() {
        let config = PrivacyConfig::default();
        assert!(config.is_invoke_allowed("any_command"));
    }

    #[test]
    fn invoke_blocked_in_observe() {
        let config = observe_privacy_config();
        assert!(!config.is_invoke_allowed("any_command"));
    }

    #[test]
    fn invoke_allowed_in_test_with_allowlist() {
        let mut allow = HashSet::new();
        allow.insert("greet".to_string());
        let mut config = test_privacy_config();
        config.command_allowlist = Some(allow);
        assert!(config.is_invoke_allowed("greet"));
        assert!(!config.is_invoke_allowed("delete_user"));
    }

    #[test]
    fn invoke_blocked_in_test_without_allowlist() {
        let config = test_privacy_config();
        assert!(!config.is_invoke_allowed("greet"));
    }

    // ── strict_privacy_config is Observe ────────────────────────────────────

    #[test]
    fn strict_privacy_is_observe_profile() {
        let config = strict_privacy_config();
        assert_eq!(config.profile, PrivacyProfile::Observe);
        assert!(config.redaction_enabled);
    }

    // ── Backward compatibility ──────────────────────────────────────────────

    #[test]
    fn strict_mode_disables_dangerous_tools() {
        let config = strict_privacy_config();
        assert!(!config.is_tool_enabled("eval_js"));
        assert!(!config.is_tool_enabled("screenshot"));
        assert!(!config.is_tool_enabled("inject_css"));
        assert!(!config.is_tool_enabled("navigate"));
        assert!(!config.is_tool_enabled("invoke_command"));
        assert!(config.is_tool_enabled("dom_snapshot"));
        assert!(config.is_tool_enabled("get_memory_stats"));
        assert!(config.redaction_enabled);
    }

    #[test]
    fn strict_mode_blocks_window_mutations() {
        let config = strict_privacy_config();
        assert!(!config.is_tool_enabled("window.manage"));
        assert!(!config.is_tool_enabled("window.resize"));
        assert!(!config.is_tool_enabled("window.move_to"));
        assert!(!config.is_tool_enabled("window.set_title"));
        assert!(config.is_tool_enabled("window"));
    }

    #[test]
    fn default_allows_all_actions() {
        let config = PrivacyConfig::default();
        assert!(config.is_tool_enabled("invoke_command"));
        assert!(config.is_tool_enabled("window.manage"));
        assert!(config.is_tool_enabled("window.resize"));
        assert!(config.is_tool_enabled("window.move_to"));
        assert!(config.is_tool_enabled("window.set_title"));
    }

    // ── Redaction ───────────────────────────────────────────────────────────

    #[test]
    fn redaction_when_enabled() {
        let config = PrivacyConfig {
            redaction_enabled: true,
            ..Default::default()
        };
        let output = config.redact_output("key is sk-abc123def456ghi789jkl012mno");
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn no_redaction_when_disabled() {
        let config = PrivacyConfig::default();
        let input = "key is sk-abc123def456ghi789jkl012mno";
        assert_eq!(config.redact_output(input), input);
    }

    // ── Display ─────────────────────────────────────────────────────────────

    #[test]
    fn profile_display() {
        assert_eq!(PrivacyProfile::Observe.to_string(), "observe");
        assert_eq!(PrivacyProfile::Test.to_string(), "test");
        assert_eq!(PrivacyProfile::FullControl.to_string(), "full_control");
    }
}
