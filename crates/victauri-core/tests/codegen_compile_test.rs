//! Codegen API drift test.
//!
//! Verifies that every method name the codegen module emits actually exists on
//! `VictauriClient`. This catches drift between codegen output and the client
//! API at compile time — no running server required.

use chrono::{Duration, Utc};
use victauri_core::codegen::CodegenOptions;
use victauri_core::event::{AppEvent, InteractionKind};
use victauri_core::recording::{RecordedEvent, RecordedSession};
use victauri_core::{generate_test, generate_test_default};

fn make_session(events: Vec<RecordedEvent>) -> RecordedSession {
    RecordedSession {
        id: "codegen-drift-test".to_string(),
        started_at: Utc::now(),
        events,
        checkpoints: vec![],
    }
}

fn interaction(
    index: usize,
    action: InteractionKind,
    selector: &str,
    value: Option<&str>,
    offset_ms: i64,
) -> RecordedEvent {
    let ts = Utc::now() + Duration::milliseconds(offset_ms);
    RecordedEvent {
        index,
        timestamp: ts,
        event: AppEvent::DomInteraction {
            action,
            selector: selector.to_string(),
            value: value.map(String::from),
            timestamp: ts,
            webview_label: "main".to_string(),
        },
    }
}

/// The canonical set of `VictauriClient` method names. If the client adds or
/// renames a method and codegen still emits the old name, this list drives the
/// assertion below.
const VALID_CLIENT_METHODS: &[&str] = &[
    // Interaction methods (ref_id-based)
    "click",
    "double_click",
    "fill",
    "hover",
    "focus",
    "press_key",
    "type_text",
    "select_option",
    "scroll_to",
    "navigate",
    // By-id variants
    "click_by_id",
    "double_click_by_id",
    "fill_by_id",
    "type_by_id",
    "select_option_by_id",
    "select_by_id",
    "scroll_to_by_id",
    // By-text variants
    "click_by_text",
    "double_click_by_text",
    "fill_by_text",
    "select_option_by_text",
    // By-selector variants (CSS selector -> find_elements -> ref_id)
    "click_by_selector",
    "double_click_by_selector",
    "fill_by_selector",
    "select_option_by_selector",
    "scroll_to_by_selector",
    // Other
    "eval_js",
    "dom_snapshot",
    "screenshot",
    "find_elements",
    "wait_for",
    "invoke_command",
    "get_ipc_log",
    "verify_state",
    "detect_ghost_commands",
    "check_ipc_integrity",
    "assert_semantic",
    "audit_accessibility",
    "get_performance_metrics",
    "get_registry",
    "get_memory_stats",
    "get_plugin_info",
    "start_recording",
    "stop_recording",
    "export_session",
    "list_windows",
    "get_window_state",
    "logs",
    "expect_text",
    "expect_text_with_timeout",
    "expect_no_text",
    "text_by_id",
    "screenshot_visual",
    "verify",
    "get_ipc_calls",
    "ipc_checkpoint",
    "ipc_calls_since",
];

/// Extracts all `client.<method>(` calls from a generated code string.
fn extract_method_calls(code: &str) -> Vec<String> {
    let mut methods = Vec::new();
    for line in code.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("client.")
            && let Some(paren) = rest.find('(')
        {
            methods.push(rest[..paren].to_string());
        }
    }
    methods
}

/// Every interaction kind, all three selector forms.
///
/// Generates code covering every `InteractionKind` variant with id, text, and
/// raw CSS selectors, then asserts every emitted method name is in the valid set.
#[test]
fn codegen_emits_only_valid_client_methods() {
    let session = make_session(vec![
        // Click variants
        interaction(0, InteractionKind::Click, "#btn-id", None, 0),
        interaction(
            1,
            InteractionKind::Click,
            "button:has-text(\"Save\")",
            None,
            10,
        ),
        interaction(2, InteractionKind::Click, "[data-testid=\"raw\"]", None, 20),
        // DoubleClick variants
        interaction(3, InteractionKind::DoubleClick, "#dbl-id", None, 30),
        interaction(
            4,
            InteractionKind::DoubleClick,
            "span:has-text(\"Edit\")",
            None,
            40,
        ),
        interaction(5, InteractionKind::DoubleClick, ".raw-dbl", None, 50),
        // Fill variants
        interaction(6, InteractionKind::Fill, "#input-id", Some("hello"), 60),
        interaction(
            7,
            InteractionKind::Fill,
            "label:has-text(\"Name\")",
            Some("world"),
            70,
        ),
        interaction(
            8,
            InteractionKind::Fill,
            "input[name=\"email\"]",
            Some("test@example.com"),
            80,
        ),
        // KeyPress (no selector variants)
        interaction(9, InteractionKind::KeyPress, "body", Some("Enter"), 90),
        // Select variants
        interaction(10, InteractionKind::Select, "#country", Some("AU"), 100),
        interaction(
            11,
            InteractionKind::Select,
            "select:has-text(\"Choose\")",
            Some("opt1"),
            110,
        ),
        interaction(12, InteractionKind::Select, "select.raw", Some("opt2"), 120),
        // Navigate
        interaction(
            13,
            InteractionKind::Navigate,
            "body",
            Some("/dashboard"),
            130,
        ),
        // Scroll variants
        interaction(14, InteractionKind::Scroll, "#scroll-target", None, 140),
        interaction(15, InteractionKind::Scroll, ".scroll-raw", None, 150),
    ]);

    let opts = CodegenOptions {
        include_timing_comments: false,
        ..CodegenOptions::default()
    };
    let code = generate_test(&session, &opts);

    let methods = extract_method_calls(&code);
    assert!(
        !methods.is_empty(),
        "codegen produced no client.* calls:\n{code}"
    );

    for method in &methods {
        assert!(
            VALID_CLIENT_METHODS.contains(&method.as_str()),
            "codegen emitted unknown method `client.{method}()` — \
             this method does not exist on VictauriClient.\n\
             Generated code:\n{code}"
        );
    }
}

/// Verify that id selectors produce `_by_id` variants, text selectors produce
/// `_by_text`, and raw selectors produce `_by_selector`.
#[test]
fn codegen_selector_resolution_is_correct() {
    let session = make_session(vec![
        interaction(0, InteractionKind::Click, "#my-id", None, 0),
        interaction(
            1,
            InteractionKind::Click,
            "button:has-text(\"Go\")",
            None,
            10,
        ),
        interaction(2, InteractionKind::Click, "[data-testid=\"raw\"]", None, 20),
    ]);

    let opts = CodegenOptions {
        include_timing_comments: false,
        ..CodegenOptions::default()
    };
    let code = generate_test(&session, &opts);

    assert!(
        code.contains("click_by_id(\"my-id\")"),
        "expected click_by_id for #my-id:\n{code}"
    );
    assert!(
        code.contains("click_by_text(\"Go\")"),
        "expected click_by_text for :has-text:\n{code}"
    );
    assert!(
        code.contains("click_by_selector(\"[data-testid="),
        "expected click_by_selector for raw selector:\n{code}"
    );
}

/// Verify `generate_test_default` works and produces a valid skeleton.
#[test]
fn codegen_default_options_produce_valid_output() {
    let session = make_session(vec![interaction(0, InteractionKind::Click, "#ok", None, 0)]);
    let code = generate_test_default(&session);

    assert!(code.contains("#[tokio::test]"));
    assert!(code.contains("async fn recorded_flow()"));
    assert!(code.contains("VictauriClient::discover()"));
    assert!(code.contains("click_by_id(\"ok\")"));
}
