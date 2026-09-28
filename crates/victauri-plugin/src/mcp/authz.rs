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

/// The standalone (non-compound) tools — gated by their bare name.
pub(crate) const STANDALONE_TOOLS: &[&str] = &[
    "eval_js",
    "dom_snapshot",
    "find_elements",
    "invoke_command",
    "screenshot",
    "verify_state",
    "detect_ghost_commands",
    "check_ipc_integrity",
    "wait_for",
    "assert_semantic",
    "resolve_command",
    "get_registry",
    "app_state",
    "get_memory_stats",
    "get_plugin_info",
    "get_diagnostics",
    "app_info",
    "list_app_dir",
    "read_app_file",
    "query_db",
];

/// Every compound tool's actions, spelled as its `action` parameter accepts them.
const COMPOUND_ACTIONS: &[(&str, &[&str])] = &[
    (
        "interact",
        &[
            "click",
            "double_click",
            "hover",
            "focus",
            "scroll_into_view",
            "select_option",
        ],
    ),
    ("input", &["fill", "type_text", "press_key"]),
    (
        "window",
        &[
            "get_state",
            "list",
            "manage",
            "resize",
            "move_to",
            "set_title",
            "introspectability",
        ],
    ),
    ("storage", &["get", "set", "delete", "get_cookies"]),
    (
        "navigate",
        &[
            "go_to",
            "go_back",
            "get_history",
            "set_dialog_response",
            "get_dialog_log",
        ],
    ),
    (
        "recording",
        &[
            "start",
            "stop",
            "checkpoint",
            "list_checkpoints",
            "get_events",
            "events_between",
            "get_replay",
            "export",
            "import",
            "replay",
            "flush",
        ],
    ),
    (
        "inspect",
        &[
            "get_styles",
            "get_bounding_boxes",
            "highlight",
            "clear_highlights",
            "audit_accessibility",
            "get_performance",
        ],
    ),
    ("css", &["inject", "remove"]),
    ("route", &["add", "list", "clear", "clear_all", "matches"]),
    ("trace", &["start", "stop", "status", "frames"]),
    ("animation", &["list", "scrub", "sample"]),
    (
        "logs",
        &[
            "console",
            "network",
            "ipc",
            "navigation",
            "dialogs",
            "events",
            "slow_ipc",
            "clear",
        ],
    ),
    (
        "introspect",
        &[
            "command_timings",
            "coverage",
            "command_catalog",
            "contract_record",
            "contract_check",
            "contract_list",
            "contract_clear",
            "startup_timing",
            "capabilities",
            "db_health",
            "plugin_state",
            "processes",
            "plugin_tasks",
            "event_bus",
            "event_bus_clear",
        ],
    ),
    ("fault", &["inject", "list", "clear", "clear_all"]),
    ("explain", &["summary", "last_action", "diff"]),
];

/// Older names some handlers (and the profile matrix) still check as defense in depth, so
/// `disabled_tools` honors them too (e.g. `"fill"` disables `input.fill`).
const LEGACY_NAMES: &[&str] = &[
    "fill",
    "type_text",
    "set_storage",
    "delete_storage",
    "get_storage",
    "get_cookies",
    "inject_css",
    "set_dialog_response",
    "get_window_state",
    "list_windows",
];

/// Capability ids whose action is spelled differently: `(capability id, tool.action)`.
/// Either spelling in `disabled_tools` disables the action (R4-NET4).
const CAPABILITY_ALIASES: &[(&str, &str)] = &[
    ("inspect.styles", "inspect.get_styles"),
    ("inspect.bounds", "inspect.get_bounding_boxes"),
    ("inspect.audit_a11y", "inspect.audit_accessibility"),
    ("inspect.performance", "inspect.get_performance"),
];

/// The other spelling of a capability whose id differs from its `tool.action` name —
/// `inspect.styles` ↔ `inspect.get_styles` — or `None` for every other name.
#[must_use]
pub(crate) fn capability_alias(name: &str) -> Option<&'static str> {
    CAPABILITY_ALIASES.iter().find_map(|&(id, action)| {
        if name == id {
            Some(action)
        } else if name == action {
            Some(id)
        } else {
            None
        }
    })
}

/// Whether `name` is something `disabled_tools` can match: a tool, a `tool.action` (either
/// spelling), a capability id, or a legacy handler name. A name that is none of these
/// disables nothing — almost always a typo.
#[must_use]
pub(crate) fn is_known_name(name: &str) -> bool {
    if STANDALONE_TOOLS.contains(&name)
        || COMPOUND_TOOLS.contains(&name)
        || LEGACY_NAMES.contains(&name)
        || capability_alias(name).is_some()
    {
        return true;
    }
    let Some((tool, action)) = name.split_once('.') else {
        return false;
    };
    COMPOUND_ACTIONS
        .iter()
        .any(|(t, actions)| *t == tool && actions.contains(&action))
}

/// The entries of `names` that match no tool, action or capability (see [`is_known_name`]).
pub(crate) fn unknown_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    names.into_iter().filter(|n| !is_known_name(n)).collect()
}

/// Returns `true` if `tool` is a compound tool (dispatches on an `action` field).
#[must_use]
pub fn is_compound_tool(tool: &str) -> bool {
    COMPOUND_TOOLS.contains(&tool)
}

/// Resolve the canonical privacy-matrix capability identity for a tool call, or
/// refuse the call as malformed.
///
/// For standalone tools this is the bare tool name. For compound tools it is the
/// dot-qualified `tool.action` identity that the privacy matrix is keyed on.
///
/// The gate and the handler must agree on WHICH action runs. serde also accepts an
/// enum written as `{"go_to": null}` (externally-tagged unit variant) and a REST body
/// written as a positional array; both used to be gated as the bare tool name —
/// which the Test profile allows for `navigate` — while the handler still parsed and
/// ran `go_to` (audit N1). So a non-object body or a non-string `action` is refused.
///
/// A missing or unknown STRING action is gated as the bare tool name, as before: the
/// handler's typed parse rejects it (listing the valid actions) before anything runs.
/// That is sound only while every action variant is mapped here, which
/// `every_action_variant_has_a_capability` pins for each compound tool's enum.
///
/// # Errors
///
/// Returns a human-readable message when the arguments are not a JSON object, or a
/// compound tool's `action` is present but not a string.
pub fn resolve_capability(tool: &str, args: &Value) -> Result<String, String> {
    if !args.is_object() {
        return Err(format!("arguments for '{tool}' must be a JSON object"));
    }
    if !is_compound_tool(tool) {
        return Ok(tool.to_string());
    }
    match args.get("action") {
        Some(Value::String(action)) => {
            Ok(action_capability(tool, action).unwrap_or_else(|| tool.to_string()))
        }
        None => Ok(tool.to_string()),
        Some(_) => Err(format!("`action` for tool '{tool}' must be a string")),
    }
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
        "introspect" => match action {
            "command_timings" | "coverage" | "command_catalog" | "contract_record"
            | "contract_check" | "contract_list" | "contract_clear" | "startup_timing"
            | "capabilities" | "db_health" | "plugin_state" | "processes" | "plugin_tasks"
            | "event_bus" | "event_bus_clear" => format!("introspect.{action}"),
            _ => return None,
        },
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

    /// Test shim: the resolved capability, panicking on a refused call.
    fn canonical_capability(tool: &str, args: &Value) -> String {
        resolve_capability(tool, args).unwrap_or_else(|e| panic!("{tool}: refused: {e}"))
    }

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
    fn missing_or_unknown_string_action_is_gated_as_the_bare_tool() {
        // The handler's typed parse then rejects it before anything runs.
        assert_eq!(canonical_capability("route", &json!({})), "route");
        assert_eq!(
            canonical_capability("route", &json!({"action": "nonsense"})),
            "route"
        );
        assert_eq!(
            canonical_capability("introspect", &json!({"action": "nonsense"})),
            "introspect"
        );
    }

    /// Audit N1: serde parses `{"go_to": null}` into `NavigateAction::GoTo`, and a
    /// REST array body positionally into the params struct. Neither may slip past
    /// the gate as the bare tool name.
    #[test]
    fn non_string_action_and_non_object_args_are_refused() {
        for (tool, action, ..) in AUTHZ_SPEC {
            let tagged = json!({ "action": { *action: null } });
            assert!(
                resolve_capability(tool, &tagged).is_err(),
                "{tool}: tagged-enum action {tagged} must be refused"
            );
            for bad in [
                json!({"action": 1}),
                json!({"action": [action]}),
                json!({"action": null}),
            ] {
                assert!(
                    resolve_capability(tool, &bad).is_err(),
                    "{tool}: {bad} must be refused"
                );
            }
            let positional = json!([action, "https://evil.example", null, null, null, null]);
            assert!(
                resolve_capability(tool, &positional).is_err(),
                "{tool}: positional array body must be refused"
            );
        }
        for body in [json!([]), json!(null), json!("eval"), json!(1)] {
            assert!(
                resolve_capability("eval_js", &body).is_err(),
                "{body} must be refused"
            );
        }
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
        // introspect — FullControl-only (all 15 actions)
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
            "command_catalog",
            "introspect.command_catalog",
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

    /// `COMPOUND_ACTIONS` (what `disabled_tools` validation accepts) lists exactly the
    /// actions in the authorization spec, and every spelling of each is a known name.
    #[test]
    fn known_names_cover_every_action_in_both_spellings() {
        let mut listed: Vec<(&str, &str)> = COMPOUND_ACTIONS
            .iter()
            .flat_map(|(t, actions)| actions.iter().map(move |a| (*t, *a)))
            .collect();
        let mut spec: Vec<(&str, &str)> = AUTHZ_SPEC.iter().map(|(t, a, ..)| (*t, *a)).collect();
        listed.sort_unstable();
        spec.sort_unstable();
        assert_eq!(listed, spec);
        for (tool, action, capability, ..) in AUTHZ_SPEC {
            assert!(is_known_name(tool), "{tool}");
            assert!(
                is_known_name(&format!("{tool}.{action}")),
                "{tool}.{action}"
            );
            assert!(is_known_name(capability), "{capability}");
        }
        for tool in STANDALONE_TOOLS {
            assert!(is_known_name(tool), "{tool}");
        }
    }

    #[test]
    fn typos_and_unknown_names_are_reported() {
        for typo in [
            "inspect.get_style",
            "evaljs",
            "screenshots",
            "logs.ipcs",
            "",
            "inspect.",
            ".styles",
            "window.list.extra",
        ] {
            assert!(!is_known_name(typo), "{typo:?} must be reported as unknown");
        }
        assert_eq!(
            unknown_names(["eval_js", "inspect.get_style", "logs", "nope"]),
            vec!["inspect.get_style", "nope"]
        );
    }

    #[test]
    fn capability_aliases_are_symmetric() {
        for (id, action) in CAPABILITY_ALIASES {
            assert_eq!(capability_alias(id), Some(*action));
            assert_eq!(capability_alias(action), Some(*id));
            // The alias really is the resolved capability of that action.
            let (tool, act) = action.split_once('.').unwrap();
            assert_eq!(action_capability(tool, act).as_deref(), Some(*id));
        }
        assert_eq!(capability_alias("inspect.highlight"), None);
        assert_eq!(capability_alias("eval_js"), None);
    }

    /// Every name a handler or the profile matrix checks is one `disabled_tools` accepts
    /// without a warning — otherwise an operator copying it would be told it is a typo.
    #[test]
    fn every_name_the_code_checks_is_known() {
        let quoted_after = |src: &str, marker: &str| -> Vec<String> {
            src.match_indices(marker)
                .filter_map(|(at, _)| {
                    let rest = &src[at + marker.len()..];
                    rest.find('"').map(|end| rest[..end].to_string())
                })
                .collect()
        };
        let handler_names = quoted_after(include_str!("mod.rs"), "is_tool_enabled(\"");
        assert!(handler_names.len() > 20, "{handler_names:?}");
        let privacy = include_str!("../privacy.rs");
        let matrix_start = privacy.find("fn is_allowed_by_profile").expect("matrix fn");
        let matrix_len = privacy[matrix_start..]
            .find("\n}\n")
            .expect("matrix fn end");
        let matrix = &privacy[matrix_start..matrix_start + matrix_len];
        let matrix_names: Vec<String> = matrix
            .split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect();
        assert!(matrix_names.len() > 50, "{matrix_names:?}");
        for name in handler_names.iter().chain(&matrix_names) {
            // `"x"` appears in a doc example in mod.rs.
            if name == "x" {
                continue;
            }
            assert!(
                is_known_name(name),
                "{name:?} is checked but not a known name"
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
