//! Centralized authorization identity resolution.
//!
//! The privacy matrix ([`crate::privacy`]) is keyed on canonical capability
//! identities: standalone tools use their bare name (`"eval_js"`), and compound
//! tools use a dot-qualified `tool.action` identity (`"window.manage"`,
//! `"inspect.styles"`). Historically each compound handler performed its own
//! ad-hoc per-action check, which meant:
//!
//! 1. actions whose handler forgot to check (e.g. `route.clear`) were reachable
//!    even when an operator put them in `disabled_tools`; and
//! 2. the central dispatch gated on the *bare* tool name, so a profile that
//!    allowed `navigate.go_back` but not the bare `navigate` tool blocked the
//!    action entirely (the matrix advertised an unreachable capability).
//!
//! [`canonical_capability`] resolves the single authoritative identity to check
//! *before* dispatch, in both the MCP and REST entry points. The handlers keep
//! their own checks as defense-in-depth, but this is the gate the negative
//! security tests target.
//!
//! IMPORTANT: the identity strings returned here MUST match the strings listed
//! in [`crate::privacy::is_allowed_by_profile`]. The action enums' `Display`
//! impls are NOT always the matrix id (e.g. inspect's `get_styles` action maps
//! to the matrix id `inspect.styles`), which is exactly why this mapping is
//! explicit rather than `format!("{tool}.{action}")`.

use serde_json::Value;

/// The set of compound tools — those that carry an `action` field and whose
/// per-action capability is gated individually.
const COMPOUND_TOOLS: &[&str] = &[
    "interact",
    "input",
    "window",
    "storage",
    "navigate",
    "recording",
    "inspect",
    "css",
    "route",
    "trace",
    "animation",
    "logs",
    "introspect",
    "fault",
    "explain",
];

/// Returns `true` if `tool` is a compound tool (dispatches on an `action` field).
#[must_use]
pub fn is_compound_tool(tool: &str) -> bool {
    COMPOUND_TOOLS.contains(&tool)
}

/// Resolve the canonical privacy-matrix capability identity for a tool call.
///
/// For standalone tools this is the bare tool name. For compound tools it is the
/// dot-qualified `tool.action` identity that the privacy matrix is keyed on. When
/// a compound tool is called without a recognizable `action`, the bare tool name
/// is returned (the per-tool arg parse will then reject the malformed call, and
/// in restricted profiles the bare name is itself not allowed — fail closed).
#[must_use]
pub fn canonical_capability(tool: &str, args: &Value) -> String {
    if !is_compound_tool(tool) {
        return tool.to_string();
    }
    let Some(action) = args.get("action").and_then(Value::as_str) else {
        return tool.to_string();
    };
    action_capability(tool, action).unwrap_or_else(|| tool.to_string())
}

/// Map a `(compound tool, action)` pair to its canonical matrix identity.
///
/// Returns `None` for an unrecognized action (caller falls back to the bare tool
/// name, which fails closed in restricted profiles).
#[must_use]
pub fn action_capability(tool: &str, action: &str) -> Option<String> {
    let id: String = match tool {
        // `interact.<action>` matches the action Display strings exactly.
        "interact" => match action {
            "click" | "double_click" | "hover" | "focus" | "scroll_into_view" | "select_option" => {
                format!("interact.{action}")
            }
            _ => return None,
        },
        "input" => match action {
            "fill" => "input.fill".into(),
            "type_text" => "input.type_text".into(),
            "press_key" => "input.press_key".into(),
            _ => return None,
        },
        "window" => match action {
            "get_state" => "window.get_state".into(),
            "list" => "window.list".into(),
            "manage" => "window.manage".into(),
            "resize" => "window.resize".into(),
            "move_to" => "window.move_to".into(),
            "set_title" => "window.set_title".into(),
            "introspectability" => "window.introspectability".into(),
            _ => return None,
        },
        "storage" => match action {
            "get" => "storage.get".into(),
            "set" => "storage.set".into(),
            "delete" => "storage.delete".into(),
            "get_cookies" => "storage.get_cookies".into(),
            _ => return None,
        },
        "navigate" => match action {
            "go_to" => "navigate.go_to".into(),
            "go_back" => "navigate.go_back".into(),
            "get_history" => "navigate.get_history".into(),
            "set_dialog_response" => "navigate.set_dialog_response".into(),
            "get_dialog_log" => "navigate.get_dialog_log".into(),
            _ => return None,
        },
        // recording.<action> matches Display strings. replay/flush are
        // deliberately FullControl-only (they re-invoke commands / drive eval),
        // so they are simply absent from the Test/Observe matrix.
        "recording" => match action {
            "start" | "stop" | "checkpoint" | "list_checkpoints" | "get_events"
            | "events_between" | "get_replay" | "export" | "import" | "replay" | "flush" => {
                format!("recording.{action}")
            }
            _ => return None,
        },
        // The inspect action Display strings differ from the matrix ids.
        "inspect" => match action {
            "get_styles" => "inspect.styles".into(),
            "get_bounding_boxes" => "inspect.bounds".into(),
            "highlight" => "inspect.highlight".into(),
            "clear_highlights" => "inspect.clear_highlights".into(),
            "audit_accessibility" => "inspect.audit_a11y".into(),
            "get_performance" => "inspect.performance".into(),
            _ => return None,
        },
        "css" => match action {
            "inject" => "css.inject".into(),
            "remove" => "css.remove".into(),
            _ => return None,
        },
        "route" => match action {
            "add" | "list" | "clear" | "clear_all" | "matches" => format!("route.{action}"),
            _ => return None,
        },
        "trace" => match action {
            "start" | "stop" | "status" | "frames" => format!("trace.{action}"),
            _ => return None,
        },
        "animation" => match action {
            "list" | "scrub" | "sample" => format!("animation.{action}"),
            _ => return None,
        },
        "logs" => match action {
            "console" | "network" | "ipc" | "navigation" | "dialogs" | "events" | "slow_ipc"
            | "clear" => format!("logs.{action}"),
            _ => return None,
        },
        "introspect" => format!("introspect.{action}"),
        "fault" => match action {
            "inject" | "list" | "clear" | "clear_all" => format!("fault.{action}"),
            _ => return None,
        },
        "explain" => match action {
            "summary" | "last_action" | "diff" => format!("explain.{action}"),
            _ => return None,
        },
        _ => return None,
    };
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn standalone_tools_use_bare_name() {
        assert_eq!(canonical_capability("eval_js", &json!({})), "eval_js");
        assert_eq!(
            canonical_capability("invoke_command", &json!({"command": "x"})),
            "invoke_command"
        );
        // A stray `action` on a standalone tool is ignored.
        assert_eq!(
            canonical_capability("screenshot", &json!({"action": "evil"})),
            "screenshot"
        );
    }

    #[test]
    fn compound_resolves_to_dotted_identity() {
        assert_eq!(
            canonical_capability("window", &json!({"action": "manage"})),
            "window.manage"
        );
        assert_eq!(
            canonical_capability("route", &json!({"action": "clear"})),
            "route.clear"
        );
        assert_eq!(
            canonical_capability("route", &json!({"action": "clear_all"})),
            "route.clear_all"
        );
        assert_eq!(
            canonical_capability("logs", &json!({"action": "clear"})),
            "logs.clear"
        );
        assert_eq!(
            canonical_capability("recording", &json!({"action": "replay"})),
            "recording.replay"
        );
    }

    #[test]
    fn inspect_action_names_map_to_matrix_ids() {
        // The action Display strings are NOT the matrix ids — verify the remap.
        assert_eq!(
            canonical_capability("inspect", &json!({"action": "get_styles"})),
            "inspect.styles"
        );
        assert_eq!(
            canonical_capability("inspect", &json!({"action": "get_bounding_boxes"})),
            "inspect.bounds"
        );
        assert_eq!(
            canonical_capability("inspect", &json!({"action": "audit_accessibility"})),
            "inspect.audit_a11y"
        );
        assert_eq!(
            canonical_capability("inspect", &json!({"action": "get_performance"})),
            "inspect.performance"
        );
    }

    #[test]
    fn missing_or_unknown_action_fails_closed_to_bare_name() {
        assert_eq!(canonical_capability("route", &json!({})), "route");
        assert_eq!(
            canonical_capability("route", &json!({"action": "nonsense"})),
            "route"
        );
    }

    #[test]
    fn every_compound_tool_is_recognized() {
        for t in COMPOUND_TOOLS {
            assert!(is_compound_tool(t), "{t} should be compound");
        }
        assert!(!is_compound_tool("eval_js"));
        assert!(!is_compound_tool("invoke_command"));
    }
}
