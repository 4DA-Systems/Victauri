//! Each test pins something the crate used to say falsely: an assertion that passed on values that
//! differ, a ranking lost to whitespace, a checkpoint window that did not say it was partial.

use chrono::Utc;
use serde_json::{Value, json};
use victauri_core::codegen::{CodegenOptions, CodegenStyle};
use victauri_core::event::{AppEvent, InteractionKind};
use victauri_core::recording::{RecordedEvent, RecordedSession, StateCheckpoint};
use victauri_core::verification::{evaluate_assertion, verify_state};
use victauri_core::{
    AssertionCondition, CommandInfo, CommandRegistry, EventRecorder, SemanticAssertion,
    generate_test,
};

fn equals(actual: Value, expected: Value) -> bool {
    let assertion = SemanticAssertion::new("x", AssertionCondition::Equals, expected);
    evaluate_assertion(actual, &assertion).passed
}

fn not_equals(actual: Value, expected: Value) -> bool {
    let assertion = SemanticAssertion::new("x", AssertionCondition::NotEquals, expected);
    evaluate_assertion(actual, &assertion).passed
}

#[test]
fn equals_does_not_call_different_values_equal() {
    let cases = [
        (json!("1.10"), json!("1.1")),
        (json!("007"), json!("7")),
        (json!("1e3"), json!("1000")),
        (json!("+5"), json!("5")),
        (json!(9007199254740993u64), json!(9007199254740992u64)),
        (json!("9007199254740993"), json!("9007199254740992")),
        (json!(9007199254740993u64), json!(9007199254740992.0)),
        (json!(1e-20), json!(2e-20)),
        (json!(null), json!("null")),
        (json!(true), json!("true")),
        (json!({"a": 1}), json!("{\"a\":1}")),
        (json!([1]), json!("[1]")),
        (json!("007"), json!(7)),
        (json!("1e3"), json!(1000)),
        (json!("1.10"), json!(1.1)),
    ];
    for (a, b) in cases {
        assert!(!equals(a.clone(), b.clone()), "equals({a}, {b})");
        assert!(not_equals(a.clone(), b.clone()), "not_equals({a}, {b})");
    }
}

#[test]
fn equals_keeps_the_comparisons_that_are_true() {
    let cases = [
        (json!(3), json!(3)),
        (json!("abc"), json!("abc")),
        (json!(null), json!(null)),
        // JavaScript has no float form.
        (json!(1), json!(1.0)),
        (json!({"n": 1}), json!({"n": 1.0})),
        (json!(0.1 + 0.2), json!(0.3)),
        // DOM reads return text.
        (json!("3"), json!(3)),
        (json!(3), json!("3")),
        (json!("3.5"), json!(3.5)),
        (json!("3.0"), json!(3)),
    ];
    for (a, b) in cases {
        assert!(equals(a.clone(), b.clone()), "equals({a}, {b})");
    }
}

#[test]
fn verify_state_treats_a_js_number_and_a_rust_float_as_the_same_value() {
    assert!(verify_state(json!({"price": 5}), json!({"price": 5.0})).passed);
    assert!(verify_state(json!({"sum": 0.1 + 0.2}), json!({"sum": 0.3})).passed);
    assert!(!verify_state(json!({"price": 5}), json!({"price": 6.0})).passed);
}

#[test]
fn exact_intent_survives_whitespace_in_the_query() {
    let registry = CommandRegistry::new();
    registry.register(CommandInfo::new("increment").with_intent("increase counter"));
    registry.register(CommandInfo::new("get_counter"));
    for query in [
        "increase counter",
        " increase counter",
        "increase  counter",
        "increase counter ",
    ] {
        let top = registry.resolve(query);
        assert_eq!(top[0].command.name, "increment", "query {query:?}");
    }
}

#[test]
fn a_checkpoint_window_says_when_it_is_partial() {
    let recorder = EventRecorder::new(5);
    recorder.start("s".into()).unwrap();
    recorder.checkpoint("A".into(), None, json!({})).unwrap();
    for i in 0..10 {
        recorder.record_event(AppEvent::console("log", format!("{i}"), Utc::now()));
    }
    recorder.checkpoint("B".into(), None, json!({})).unwrap();
    recorder.record_event(AppEvent::console("log", "after", Utc::now()));
    recorder.checkpoint("C".into(), None, json!({})).unwrap();

    let evicted = recorder.checkpoint_window("A", "B").unwrap();
    assert_eq!((evicted.events.len(), evicted.dropped), (4, 6));
    let complete = recorder.checkpoint_window("B", "C").unwrap();
    assert_eq!((complete.events.len(), complete.dropped), (1, 0));
}

#[test]
fn a_repeated_checkpoint_id_is_refused() {
    let recorder = EventRecorder::new(100);
    recorder.start("s".into()).unwrap();
    recorder.checkpoint("A".into(), None, json!({})).unwrap();
    recorder.record_event(AppEvent::console("log", "x", Utc::now()));
    let second = recorder.checkpoint("A".into(), None, json!({}));
    assert!(second.is_err(), "a second checkpoint 'A' was accepted");
    assert_eq!(recorder.checkpoint_count(), 1);
}

#[test]
fn import_continues_past_every_index_the_session_used() {
    let now = Utc::now();
    let event =
        |i: usize| RecordedEvent::new(i, now, AppEvent::console("log", format!("{i}"), now));
    let recorder = EventRecorder::new(100);
    recorder.import(RecordedSession::new(
        "s",
        now,
        vec![event(5), event(2)],
        vec![],
    ));
    recorder.record_event(AppEvent::console("log", "new", now));
    let indexes: Vec<usize> = recorder
        .export()
        .unwrap()
        .events
        .iter()
        .map(|e| e.index)
        .collect();
    assert_eq!(indexes, [5, 2, 6]);

    // `StateCheckpoint` cannot be built outside the crate except through JSON.
    let checkpoint: StateCheckpoint = serde_json::from_value(json!({
        "id": "late", "label": null, "timestamp": now, "state": {}, "event_index": 50
    }))
    .unwrap();
    let recorder = EventRecorder::new(100);
    recorder.import(RecordedSession::new("s", now, vec![], vec![checkpoint]));
    recorder.record_event(AppEvent::console("log", "new", now));
    assert_eq!(recorder.export().unwrap().events[0].index, 50);
}

fn generated(test_name: &str, selector: &str, style: CodegenStyle) -> String {
    let now = Utc::now();
    let session = RecordedSession::new(
        "s",
        now,
        vec![RecordedEvent::new(
            0,
            now,
            AppEvent::dom_interaction(InteractionKind::Click, selector, None, now, "main"),
        )],
        vec![],
    );
    let mut options = CodegenOptions::default();
    options.test_name = test_name.to_string();
    options.style = style;
    generate_test(&session, &options)
}

#[test]
fn a_generated_test_name_is_always_an_identifier() {
    for (name, expected) in [
        ("-", "recorded_flow"),
        ("!", "recorded_flow"),
        ("_", "recorded_flow"),
        ("self", "recorded_self"),
        ("Self", "recorded_Self"),
    ] {
        let code = generated(name, "#x", CodegenStyle::Direct);
        assert!(code.contains(&format!("async fn {expected}()")), "{code}");
    }
}

#[test]
fn locator_codegen_keeps_the_recorded_role_and_tag() {
    let code = generated(
        "t",
        "[role=\"button\"]:has-text(\"Save\")",
        CodegenStyle::Locator,
    );
    assert!(
        code.contains("Locator::role(\"button\").and_text(\"Save\").click("),
        "{code}"
    );
    let code = generated("t", "button:has-text(\"Save\")", CodegenStyle::Locator);
    assert!(
        code.contains("Locator::text(\"Save\").and_tag(\"button\").click("),
        "{code}"
    );
}
