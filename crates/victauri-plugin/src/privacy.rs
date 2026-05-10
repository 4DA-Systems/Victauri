use std::collections::HashSet;

use crate::redaction::Redactor;

/// Privacy controls for the MCP server. Blocklist takes precedence over allowlist.
#[derive(Default)]
pub struct PrivacyConfig {
    /// If set, only these commands can be invoked (positive allowlist).
    pub command_allowlist: Option<HashSet<String>>,
    /// Commands that are always blocked, even if on the allowlist.
    pub command_blocklist: HashSet<String>,
    /// MCP tool names that are disabled (e.g., `"eval_js"`, `"screenshot"`).
    pub disabled_tools: HashSet<String>,
    /// Output redactor with regex and JSON-key matching.
    pub redactor: Redactor,
    /// Whether output redaction is active.
    pub redaction_enabled: bool,
}

impl PrivacyConfig {
    /// Returns `true` if the command passes both the allowlist and blocklist checks.
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

    /// Returns `true` unless the tool is in [`disabled_tools`](Self::disabled_tools).
    #[must_use]
    pub fn is_tool_enabled(&self, tool_name: &str) -> bool {
        !self.disabled_tools.contains(tool_name)
    }

    /// Apply redaction rules to the output string if redaction is enabled, otherwise pass through.
    #[must_use]
    pub fn redact_output(&self, output: &str) -> String {
        if self.redaction_enabled {
            self.redactor.redact(output)
        } else {
            output.to_string()
        }
    }
}

const STRICT_DISABLED_TOOLS: &[&str] = &[
    "eval_js",
    "screenshot",
    "inject_css",
    "set_storage",
    "delete_storage",
    "navigate",
    "set_dialog_response",
    "fill",
    "type_text",
    "invoke_command",
    "window.manage",
    "window.resize",
    "window.move_to",
    "window.set_title",
];

/// Create a [`PrivacyConfig`] that disables dangerous tools (eval, screenshot, mutations) and enables redaction.
pub fn strict_privacy_config() -> PrivacyConfig {
    PrivacyConfig {
        command_allowlist: None,
        command_blocklist: HashSet::new(),
        disabled_tools: STRICT_DISABLED_TOOLS
            .iter()
            .map(std::string::ToString::to_string)
            .collect(),
        redactor: Redactor::default(),
        redaction_enabled: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn tool_disabling() {
        let mut disabled = HashSet::new();
        disabled.insert("eval_js".to_string());
        let config = PrivacyConfig {
            disabled_tools: disabled,
            ..Default::default()
        };
        assert!(!config.is_tool_enabled("eval_js"));
        assert!(config.is_tool_enabled("dom_snapshot"));
    }

    #[test]
    fn strict_mode_disables_dangerous_tools() {
        let config = strict_privacy_config();
        assert!(!config.is_tool_enabled("eval_js"));
        assert!(!config.is_tool_enabled("screenshot"));
        assert!(!config.is_tool_enabled("inject_css"));
        assert!(!config.is_tool_enabled("navigate"));
        assert!(!config.is_tool_enabled("invoke_command"));
        assert!(config.is_tool_enabled("dom_snapshot"));
        assert!(config.is_tool_enabled("get_window_state"));
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
}
