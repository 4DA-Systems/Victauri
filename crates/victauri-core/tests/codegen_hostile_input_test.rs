//! `victauri record` turns recorded page activity — attacker-controllable text —
//! into Rust source. Whatever the page recorded, the generated file must be valid
//! Rust that rustc accepts and that says what the recording said.

use chrono::{Duration, Utc};
use victauri_core::codegen::{CodegenOptions, CodegenStyle};
use victauri_core::event::{AppEvent, InteractionKind, IpcCall, IpcResult};
use victauri_core::generate_test;
use victauri_core::recording::{RecordedEvent, RecordedSession};

/// Characters rustc rejects outright in comments and literals
/// (`text_direction_codepoint_in_{comment,literal}` are deny-by-default).
const BIDI: &[char] = &[
    '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}', '\u{2067}', '\u{2068}',
    '\u{2069}',
];

fn interaction(action: InteractionKind, selector: &str, value: Option<&str>) -> RecordedEvent {
    let ts = Utc::now() + Duration::milliseconds(1);
    RecordedEvent::new(
        0,
        ts,
        AppEvent::dom_interaction(
            action,
            selector.to_string(),
            value.map(String::from),
            ts,
            "main".to_string(),
        ),
    )
}

fn session(id: &str, events: Vec<RecordedEvent>) -> RecordedSession {
    RecordedSession::new(id.to_string(), Utc::now(), events, vec![])
}

fn both_styles(s: &RecordedSession) -> Vec<String> {
    [CodegenStyle::Direct, CodegenStyle::Locator]
        .into_iter()
        .map(|style| {
            let options = CodegenOptions {
                style,
                emit_ipc_assert_calls: true,
                ..CodegenOptions::default()
            };
            generate_test(s, &options)
        })
        .collect()
}

fn assert_clean(code: &str) {
    assert!(
        !code
            .chars()
            .any(|c| BIDI.contains(&c) || (c.is_control() && c != '\n')),
        "generated code carries a raw control/bidi character:\n{code}"
    );
}

#[test]
fn bidi_and_control_characters_are_escaped_everywhere() {
    let evil = "ok\u{202e}gnp.exe\u{2066}\u{0}\u{7}";
    let ipc = IpcCall::new(
        "c1",
        format!("save{evil}"),
        Utc::now(),
        IpcResult::Ok(serde_json::json!(null)),
        None,
        0,
        "main",
    );
    let s = session(
        &format!("sess{evil}"),
        vec![
            interaction(InteractionKind::Click, &format!("#btn{evil}"), None),
            interaction(
                InteractionKind::Fill,
                &format!("button:has-text(\"{evil}\")"),
                Some(evil),
            ),
            interaction(InteractionKind::Navigate, "", Some(evil)),
            RecordedEvent::new(1, Utc::now(), AppEvent::Ipc(ipc)),
        ],
    );
    for code in both_styles(&s) {
        assert_clean(&code);
        assert!(code.contains("\\u{202e}"), "escape is visible:\n{code}");
    }
}

#[test]
fn a_quoted_value_cannot_masquerade_as_a_has_text_selector() {
    // The test id itself contains the `:has-text("` marker the old parser searched for.
    let s = session(
        "s",
        vec![interaction(
            InteractionKind::Click,
            r#"[data-testid="a:has-text(\"x\")"]"#,
            None,
        )],
    );
    let direct = &both_styles(&s)[0];
    assert!(
        direct.contains(r#"click_by_selector("[data-testid=\"a:has-text(\\\"x\\\")\"]")"#),
        "{direct}"
    );
    let locator = &both_styles(&s)[1];
    assert!(
        locator.contains(r#"Locator::test_id("a:has-text(\"x\")")"#),
        "{locator}"
    );
}

#[test]
fn only_a_whole_id_selector_becomes_by_id() {
    for (selector, expected) in [
        ("#save", r#"click_by_id("save")"#),
        // CSS.escape'd React useId id: decoded for the id lookup.
        (r"#\:r0\:", r#"click_by_id(":r0:")"#),
        (r"#\31 23", r#"click_by_id("123")"#),
        // Compound selectors are not ids: `#a.b` is id "a" AND class "b".
        ("#a.b", r##"click_by_selector("#a.b")"##),
        ("#a>b", r##"click_by_selector("#a>b")"##),
        ("#a:hover", r##"click_by_selector("#a:hover")"##),
    ] {
        let s = session(
            "s",
            vec![interaction(InteractionKind::Click, selector, None)],
        );
        let direct = &both_styles(&s)[0];
        assert!(direct.contains(expected), "{selector}:\n{direct}");
    }
}

#[test]
fn escaped_text_is_decoded_and_re_escaped() {
    // bestSelector writes CSS strings: `\"`, `\\`, and hex escapes for controls.
    let s = session(
        "s",
        vec![interaction(
            InteractionKind::Click,
            r#"[role="tab"]:has-text("say \"hi\"\a back\\slash")"#,
            None,
        )],
    );
    let direct = &both_styles(&s)[0];
    assert!(
        direct.contains(r#"click_by_text("say \"hi\"\nback\\slash")"#),
        "{direct}"
    );
}

#[test]
fn scrolling_to_text_uses_the_text_lookup_not_a_css_selector() {
    let s = session(
        "s",
        vec![interaction(
            InteractionKind::Scroll,
            r#"h2:has-text("Pricing")"#,
            None,
        )],
    );
    let direct = &both_styles(&s)[0];
    assert!(
        direct.contains(r#"scroll_to_by_text("Pricing")"#),
        "{direct}"
    );
}
