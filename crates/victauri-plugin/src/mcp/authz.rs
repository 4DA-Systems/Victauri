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

    // The COMPLETE authorization spec: every compound (tool, action) pair, its
    // canonical capability id, and whether it is permitted in the Observe / Test
    // profiles (FullControl permits everything). This table is the single source of
    // truth — if a new action variant is added to a compound tool's enum and not
    // mapped here (and in `action_capability` + the privacy matrix), the tests below
    // fail. It proves there is no unmapped action silently falling back to the bare
    // tool name, and no profile mismatch.
    //
    // Columns: (tool, action, expected_capability, allowed_in_observe, allowed_in_test)
    const AUTHZ_SPEC: &[(&str, &str, &str, bool, bool)] = &[
        // interact — all FullControl/Test, never Observe (mutating)
        ("interact", "click", "interact.click", false, true),
        (
            "interact",
            "double_click",
            "interact.double_click",
            false,
            true,
        ),
        ("interact", "hover", "interact.hover", false, true),
        ("interact", "focus", "interact.focus", false, true),
        (
            "interact",
            "scroll_into_view",
            "interact.scroll_into_view",
            false,
            true,
        ),
        (
            "interact",
            "select_option",
            "interact.select_option",
            false,
            true,
        ),
        // input
        ("input", "fill", "input.fill", false, true),
        ("input", "type_text", "input.type_text", false, true),
        ("input", "press_key", "input.press_key", false, true),
        // window — reads allowed in Observe+Test; mutations FullControl-only
        ("window", "get_state", "window.get_state", true, true),
        ("window", "list", "window.list", true, true),
        (
            "window",
            "introspectability",
            "window.introspectability",
            true,
            true,
        ),
        ("window", "manage", "window.manage", false, false),
        ("window", "resize", "window.resize", false, false),
        ("window", "move_to", "window.move_to", false, false),
        ("window", "set_title", "window.set_title", false, false),
        // storage — writes+reads in Test, nothing in Observe
        ("storage", "get", "storage.get", false, true),
        ("storage", "set", "storage.set", false, true),
        ("storage", "delete", "storage.delete", false, true),
        ("storage", "get_cookies", "storage.get_cookies", false, true),
        // navigate — reads in Test (B4 fix), mutations FullControl-only
        ("navigate", "go_to", "navigate.go_to", false, false),
        ("navigate", "go_back", "navigate.go_back", false, true),
        (
            "navigate",
            "get_history",
            "navigate.get_history",
            false,
            true,
        ),
        (
            "navigate",
            "set_dialog_response",
            "navigate.set_dialog_response",
            false,
            false,
        ),
        (
            "navigate",
            "get_dialog_log",
            "navigate.get_dialog_log",
            false,
            true,
        ),
        // recording — Test except replay/flush (FullControl-only)
        ("recording", "start", "recording.start", false, true),
        ("recording", "stop", "recording.stop", false, true),
        (
            "recording",
            "checkpoint",
            "recording.checkpoint",
            false,
            true,
        ),
        (
            "recording",
            "list_checkpoints",
            "recording.list_checkpoints",
            false,
            true,
        ),
        (
            "recording",
            "get_events",
            "recording.get_events",
            false,
            true,
        ),
        (
            "recording",
            "events_between",
            "recording.events_between",
            false,
            true,
        ),
        (
            "recording",
            "get_replay",
            "recording.get_replay",
            false,
            true,
        ),
        ("recording", "export", "recording.export", false, true),
        ("recording", "import", "recording.import", false, true),
        ("recording", "replay", "recording.replay", false, false),
        ("recording", "flush", "recording.flush", false, false),
        // inspect — reads in Observe+Test; highlight/clear_highlights Test-only
        ("inspect", "get_styles", "inspect.styles", true, true),
        (
            "inspect",
            "get_bounding_boxes",
            "inspect.bounds",
            true,
            true,
        ),
        ("inspect", "highlight", "inspect.highlight", false, true),
        (
            "inspect",
            "clear_highlights",
            "inspect.clear_highlights",
            false,
            true,
        ),
        (
            "inspect",
            "audit_accessibility",
            "inspect.audit_a11y",
            true,
            true,
        ),
        (
            "inspect",
            "get_performance",
            "inspect.performance",
            true,
            true,
        ),
        // css — FullControl-only
        ("css", "inject", "css.inject", false, false),
        ("css", "remove", "css.remove", false, false),
        // route — FullControl-only (every action, incl. the historically-ungated clear)
        ("route", "add", "route.add", false, false),
        ("route", "list", "route.list", false, false),
        ("route", "clear", "route.clear", false, false),
        ("route", "clear_all", "route.clear_all", false, false),
        ("route", "matches", "route.matches", false, false),
        // trace — FullControl-only
        ("trace", "start", "trace.start", false, false),
        ("trace", "stop", "trace.stop", false, false),
        ("trace", "status", "trace.status", false, false),
        ("trace", "frames", "trace.frames", false, false),
        // animation — FullControl-only
        ("animation", "list", "animation.list", false, false),
        ("animation", "scrub", "animation.scrub", false, false),
        ("animation", "sample", "animation.sample", false, false),
        // logs — reads in Observe+Test; clear Test-only
        ("logs", "console", "logs.console", true, true),
        ("logs", "network", "logs.network", true, true),
        ("logs", "ipc", "logs.ipc", true, true),
        ("logs", "navigation", "logs.navigation", true, true),
        ("logs", "dialogs", "logs.dialogs", true, true),
        ("logs", "events", "logs.events", true, true),
        ("logs", "slow_ipc", "logs.slow_ipc", true, true),
        ("logs", "clear", "logs.clear", false, true),
        // introspect — FullControl-only (all 14 actions)
        (
            "introspect",
            "command_timings",
            "introspect.command_timings",
            false,
            false,
        ),
        (
            "introspect",
            "coverage",
            "introspect.coverage",
            false,
            false,
        ),
        (
            "introspect",
            "contract_record",
            "introspect.contract_record",
            false,
            false,
        ),
        (
            "introspect",
            "contract_check",
            "introspect.contract_check",
            false,
            false,
        ),
        (
            "introspect",
            "contract_list",
            "introspect.contract_list",
            false,
            false,
        ),
        (
            "introspect",
            "contract_clear",
            "introspect.contract_clear",
            false,
            false,
        ),
        (
            "introspect",
            "startup_timing",
            "introspect.startup_timing",
            false,
            false,
        ),
        (
            "introspect",
            "capabilities",
            "introspect.capabilities",
            false,
            false,
        ),
        (
            "introspect",
            "db_health",
            "introspect.db_health",
            false,
            false,
        ),
        (
            "introspect",
            "plugin_state",
            "introspect.plugin_state",
            false,
            false,
        ),
        (
            "introspect",
            "processes",
            "introspect.processes",
            false,
            false,
        ),
        (
            "introspect",
            "plugin_tasks",
            "introspect.plugin_tasks",
            false,
            false,
        ),
        (
            "introspect",
            "event_bus",
            "introspect.event_bus",
            false,
            false,
        ),
        (
            "introspect",
            "event_bus_clear",
            "introspect.event_bus_clear",
            false,
            false,
        ),
        // fault — FullControl-only
        ("fault", "inject", "fault.inject", false, false),
        ("fault", "list", "fault.list", false, false),
        ("fault", "clear", "fault.clear", false, false),
        ("fault", "clear_all", "fault.clear_all", false, false),
        // explain — FullControl-only
        ("explain", "summary", "explain.summary", false, false),
        (
            "explain",
            "last_action",
            "explain.last_action",
            false,
            false,
        ),
        ("explain", "diff", "explain.diff", false, false),
    ];

    #[test]
    fn authz_spec_is_complete_and_correct() {
        use crate::privacy::{PrivacyConfig, observe_privacy_config, test_privacy_config};
        let observe = observe_privacy_config();
        let test = test_privacy_config();
        let full = PrivacyConfig::default();

        for &(tool, action, expected_cap, observe_ok, test_ok) in AUTHZ_SPEC {
            // 1. The resolver must map to the canonical id (never the bare fallback).
            let resolved = canonical_capability(tool, &json!({ "action": action }));
            assert_eq!(
                resolved, expected_cap,
                "{tool}.{action} resolved to {resolved:?}, expected {expected_cap:?}"
            );
            assert!(
                resolved.contains('.'),
                "{tool}.{action} fell back to a bare name ({resolved}) — unmapped action"
            );
            // 2. Profile semantics must match the spec exactly.
            assert_eq!(
                observe.is_tool_enabled(expected_cap),
                observe_ok,
                "Observe profile mismatch for {expected_cap}"
            );
            assert_eq!(
                test.is_tool_enabled(expected_cap),
                test_ok,
                "Test profile mismatch for {expected_cap}"
            );
            // 3. FullControl always permits.
            assert!(
                full.is_tool_enabled(expected_cap),
                "FullControl must permit {expected_cap}"
            );
        }
    }

    #[test]
    fn authz_spec_covers_every_compound_tool() {
        // Guards against adding a compound tool to COMPOUND_TOOLS but forgetting to
        // spec its actions here (which would let an action go untested).
        for tool in COMPOUND_TOOLS {
            assert!(
                AUTHZ_SPEC.iter().any(|(t, ..)| t == tool),
                "compound tool {tool} has no entries in AUTHZ_SPEC"
            );
        }
    }
}
