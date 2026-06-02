use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::sync::Arc;
use victauri_core::*;

#[test]
fn event_log_push_and_snapshot() {
    let log = EventLog::new(100);
    assert!(log.is_empty());

    log.push(AppEvent::Ipc(IpcCall {
        id: "1".to_string(),
        command: "test_cmd".to_string(),
        timestamp: Utc::now(),
        duration_ms: Some(5),
        result: event::IpcResult::Ok(serde_json::json!(42)),
        arg_size_bytes: 10,
        webview_label: "main".to_string(),
    }));

    assert_eq!(log.len(), 1);
    assert!(!log.is_empty());

    let snapshot = log.snapshot();
    assert_eq!(snapshot.len(), 1);
    match &snapshot[0] {
        AppEvent::Ipc(call) => {
            assert_eq!(call.command, "test_cmd");
            assert_eq!(call.webview_label, "main");
        }
        _ => panic!("expected Ipc event"),
    }
}

#[test]
fn event_log_ring_buffer_eviction() {
    let log = EventLog::new(3);

    for i in 0..5 {
        log.push(AppEvent::Ipc(IpcCall {
            id: i.to_string(),
            command: format!("cmd_{i}"),
            timestamp: Utc::now(),
            duration_ms: None,
            result: event::IpcResult::Pending,
            arg_size_bytes: 0,
            webview_label: "main".to_string(),
        }));
    }

    assert_eq!(log.len(), 3);
    let calls = log.ipc_calls();
    assert_eq!(calls[0].command, "cmd_2");
    assert_eq!(calls[1].command, "cmd_3");
    assert_eq!(calls[2].command, "cmd_4");
}

#[test]
fn event_log_ipc_calls_filter() {
    let log = EventLog::new(100);

    log.push(AppEvent::Ipc(IpcCall {
        id: "1".to_string(),
        command: "save".to_string(),
        timestamp: Utc::now(),
        duration_ms: None,
        result: event::IpcResult::Pending,
        arg_size_bytes: 0,
        webview_label: "main".to_string(),
    }));

    log.push(AppEvent::StateChange {
        key: "user".to_string(),
        timestamp: Utc::now(),
        caused_by: None,
    });

    log.push(AppEvent::Ipc(IpcCall {
        id: "2".to_string(),
        command: "load".to_string(),
        timestamp: Utc::now(),
        duration_ms: Some(10),
        result: event::IpcResult::Ok(serde_json::json!("data")),
        arg_size_bytes: 5,
        webview_label: "main".to_string(),
    }));

    assert_eq!(log.len(), 3);
    let ipc_only = log.ipc_calls();
    assert_eq!(ipc_only.len(), 2);
    assert_eq!(ipc_only[0].command, "save");
    assert_eq!(ipc_only[1].command, "load");
}

#[test]
fn event_log_clear() {
    let log = EventLog::new(100);
    log.push(AppEvent::WindowEvent {
        label: "main".to_string(),
        event: "focus".to_string(),
        timestamp: Utc::now(),
    });
    assert_eq!(log.len(), 1);
    log.clear();
    assert!(log.is_empty());
}

#[test]
fn command_registry_register_and_list() {
    let registry = CommandRegistry::new();
    assert_eq!(registry.count(), 0);

    let mut cmd = CommandInfo::new("save_file").with_description("Save a file to disk");
    cmd.args = vec![CommandArg {
        name: "path".to_string(),
        type_name: "String".to_string(),
        required: true,
        schema: None,
    }];
    cmd.return_type = Some("Result<(), String>".to_string());
    cmd.is_async = true;
    registry.register(cmd);

    assert_eq!(registry.count(), 1);
    let cmd = registry.get("save_file").unwrap();
    assert_eq!(cmd.description.as_deref(), Some("Save a file to disk"));
    assert_eq!(cmd.args.len(), 1);
    assert!(cmd.is_async);
}

#[test]
fn command_registry_search() {
    let registry = CommandRegistry::new();

    registry.register(CommandInfo::new("get_users").with_description("Fetch all users"));

    registry.register(CommandInfo::new("save_settings").with_description("Save app settings"));

    registry.register(CommandInfo::new("delete_user").with_description("Remove a user"));

    let results = registry.search("user");
    assert_eq!(results.len(), 2);

    let results = registry.search("save");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "save_settings");

    let results = registry.search("nonexistent");
    assert!(results.is_empty());
}

#[test]
fn dom_snapshot_accessible_text() {
    let snapshot = DomSnapshot {
        webview_label: "main".to_string(),
        elements: vec![DomElement {
            ref_id: "e0".to_string(),
            tag: "div".to_string(),
            role: Some("main".to_string()),
            name: Some("Content".to_string()),
            text: None,
            value: None,
            enabled: true,
            visible: true,
            focusable: false,
            bounds: None,
            children: vec![
                DomElement {
                    ref_id: "e1".to_string(),
                    tag: "button".to_string(),
                    role: Some("button".to_string()),
                    name: Some("Submit".to_string()),
                    text: Some("Submit".to_string()),
                    value: None,
                    enabled: true,
                    visible: true,
                    focusable: true,
                    bounds: None,
                    children: vec![],
                    attributes: BTreeMap::new(),
                },
                DomElement {
                    ref_id: "e2".to_string(),
                    tag: "input".to_string(),
                    role: Some("textbox".to_string()),
                    name: Some("Email".to_string()),
                    text: None,
                    value: None,
                    enabled: true,
                    visible: true,
                    focusable: true,
                    bounds: None,
                    children: vec![],
                    attributes: BTreeMap::new(),
                },
            ],
            attributes: BTreeMap::new(),
        }],
        ref_map: BTreeMap::new(),
    };

    let text = snapshot.to_accessible_text(0);
    assert!(text.contains("main \"Content\""));
    assert!(text.contains("button \"Submit\" [ref=e1]"));
    assert!(text.contains("textbox \"Email\" [ref=e2]"));
}

#[test]
fn dom_snapshot_hides_invisible() {
    let snapshot = DomSnapshot {
        webview_label: "main".to_string(),
        elements: vec![DomElement {
            ref_id: "e0".to_string(),
            tag: "div".to_string(),
            role: None,
            name: None,
            text: None,
            value: None,
            enabled: true,
            visible: false,
            focusable: false,
            bounds: None,
            children: vec![],
            attributes: BTreeMap::new(),
        }],
        ref_map: BTreeMap::new(),
    };

    let text = snapshot.to_accessible_text(0);
    assert!(text.is_empty());
}

#[test]
fn window_state_serialization() {
    let state = WindowState {
        label: "main".to_string(),
        title: "My App".to_string(),
        url: "tauri://localhost".to_string(),
        visible: true,
        focused: true,
        maximized: false,
        minimized: false,
        fullscreen: false,
        position: (100, 200),
        size: (800, 600),
    };

    let json = serde_json::to_string(&state).unwrap();
    let deserialized: WindowState = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.label, "main");
    assert_eq!(deserialized.size, (800, 600));
    assert!(deserialized.visible);
}

#[test]
fn verification_result_with_divergences() {
    use victauri_core::types::{Divergence, DivergenceSeverity, VerificationResult};

    let result = VerificationResult {
        passed: false,
        frontend_state: serde_json::json!({"count": 5}),
        backend_state: serde_json::json!({"count": 3}),
        divergences: vec![Divergence {
            path: "count".to_string(),
            frontend_value: serde_json::json!(5),
            backend_value: serde_json::json!(3),
            severity: DivergenceSeverity::Error,
        }],
    };

    assert!(!result.passed);
    assert_eq!(result.divergences.len(), 1);
    assert_eq!(result.divergences[0].path, "count");
}

// ── Phase 2: Cross-boundary state verification ─────────────────────────────

#[test]
fn verify_state_identical() {
    let state = serde_json::json!({"count": 5, "name": "test"});
    let result = victauri_core::verify_state(state.clone(), state);
    assert!(result.passed);
    assert!(result.divergences.is_empty());
}

#[test]
fn verify_state_scalar_divergence() {
    let frontend = serde_json::json!({"count": 5});
    let backend = serde_json::json!({"count": 3});
    let result = victauri_core::verify_state(frontend, backend);

    assert!(!result.passed);
    assert_eq!(result.divergences.len(), 1);
    assert_eq!(result.divergences[0].path, "count");
    assert_eq!(result.divergences[0].frontend_value, serde_json::json!(5));
    assert_eq!(result.divergences[0].backend_value, serde_json::json!(3));
}

#[test]
fn verify_state_missing_keys() {
    let frontend = serde_json::json!({"a": 1, "b": 2});
    let backend = serde_json::json!({"b": 2, "c": 3});
    let result = victauri_core::verify_state(frontend, backend);

    assert!(!result.passed);
    assert_eq!(result.divergences.len(), 2);
    let paths: Vec<&str> = result.divergences.iter().map(|d| d.path.as_str()).collect();
    assert!(paths.contains(&"a"));
    assert!(paths.contains(&"c"));
}

#[test]
fn verify_state_nested_objects() {
    let frontend = serde_json::json!({"user": {"name": "Alice", "age": 30}});
    let backend = serde_json::json!({"user": {"name": "Alice", "age": 31}});
    let result = victauri_core::verify_state(frontend, backend);

    assert!(!result.passed);
    assert_eq!(result.divergences.len(), 1);
    assert_eq!(result.divergences[0].path, "user.age");
}

#[test]
fn verify_state_array_divergence() {
    let frontend = serde_json::json!({"items": [1, 2, 3]});
    let backend = serde_json::json!({"items": [1, 2, 4]});
    let result = victauri_core::verify_state(frontend, backend);

    assert!(!result.passed);
    assert_eq!(result.divergences.len(), 1);
    assert_eq!(result.divergences[0].path, "items[2]");
}

#[test]
fn verify_state_array_length_mismatch() {
    let frontend = serde_json::json!({"items": [1, 2, 3]});
    let backend = serde_json::json!({"items": [1, 2]});
    let result = victauri_core::verify_state(frontend, backend);

    assert!(!result.passed);
    assert_eq!(result.divergences.len(), 1);
    assert_eq!(result.divergences[0].path, "items[2]");
    assert_eq!(result.divergences[0].backend_value, serde_json::Value::Null);
}

#[test]
fn verify_state_type_mismatch() {
    let frontend = serde_json::json!({"value": "five"});
    let backend = serde_json::json!({"value": 5});
    let result = victauri_core::verify_state(frontend, backend);

    assert!(!result.passed);
    assert_eq!(result.divergences.len(), 1);
    assert!(matches!(
        result.divergences[0].severity,
        victauri_core::types::DivergenceSeverity::Error
    ));
}

// ── Phase 2: Ghost command detection ────────────────────────────────────────

#[test]
fn ghost_commands_all_matched() {
    let registry = CommandRegistry::new();
    registry.register(CommandInfo::new("save"));
    registry.register(CommandInfo::new("load"));

    let frontend = vec!["save".to_string(), "load".to_string()];
    let report = victauri_core::detect_ghost_commands(&frontend, &registry);

    assert!(report.frontend_only.is_empty());
    assert!(report.registry_only.is_empty());
    assert_eq!(report.total_frontend_commands, 2);
    assert_eq!(report.total_registry_commands, 2);
}

#[test]
fn ghost_commands_frontend_only() {
    let registry = CommandRegistry::new();
    registry.register(CommandInfo::new("save"));

    let frontend = vec!["save".to_string(), "unknown_cmd".to_string()];
    let report = victauri_core::detect_ghost_commands(&frontend, &registry);

    assert_eq!(report.frontend_only.len(), 1);
    assert_eq!(report.frontend_only[0].name, "unknown_cmd");
    assert!(matches!(
        report.frontend_only[0].source,
        victauri_core::GhostSource::FrontendOnly
    ));
}

#[test]
fn ghost_commands_registry_only() {
    let registry = CommandRegistry::new();
    registry.register(CommandInfo::new("save").with_description("Save data"));
    registry.register(CommandInfo::new("unused_cmd").with_description("Never called"));

    let frontend = vec!["save".to_string()];
    let report = victauri_core::detect_ghost_commands(&frontend, &registry);

    assert!(report.frontend_only.is_empty());
    assert_eq!(report.registry_only.len(), 1);
    assert_eq!(report.registry_only[0].name, "unused_cmd");
    assert!(matches!(
        report.registry_only[0].source,
        victauri_core::GhostSource::RegistryOnly
    ));
}

#[test]
fn ghost_commands_bidirectional() {
    let registry = CommandRegistry::new();
    registry.register(CommandInfo::new("shared"));
    registry.register(CommandInfo::new("backend_only"));

    let frontend = vec!["shared".to_string(), "frontend_only".to_string()];
    let report = victauri_core::detect_ghost_commands(&frontend, &registry);

    assert_eq!(report.frontend_only.len(), 1);
    assert_eq!(report.registry_only.len(), 1);
    let frontend_names: Vec<&str> = report
        .frontend_only
        .iter()
        .map(|g| g.name.as_str())
        .collect();
    let registry_names: Vec<&str> = report
        .registry_only
        .iter()
        .map(|g| g.name.as_str())
        .collect();
    assert!(registry_names.contains(&"backend_only"));
    assert!(frontend_names.contains(&"frontend_only"));
}

// ── Phase 2: IPC round-trip integrity ───────────────────────────────────────

#[test]
fn ipc_integrity_healthy() {
    let log = EventLog::new(100);
    log.push(AppEvent::Ipc(IpcCall {
        id: "1".to_string(),
        command: "save".to_string(),
        timestamp: Utc::now(),
        duration_ms: Some(5),
        result: event::IpcResult::Ok(serde_json::json!("ok")),
        arg_size_bytes: 10,
        webview_label: "main".to_string(),
    }));
    log.push(AppEvent::Ipc(IpcCall {
        id: "2".to_string(),
        command: "load".to_string(),
        timestamp: Utc::now(),
        duration_ms: Some(3),
        result: event::IpcResult::Ok(serde_json::json!("data")),
        arg_size_bytes: 0,
        webview_label: "main".to_string(),
    }));

    let report = victauri_core::check_ipc_integrity(&log, 5000);
    assert!(report.healthy);
    assert_eq!(report.total_calls, 2);
    assert_eq!(report.completed, 2);
    assert_eq!(report.pending, 0);
    assert_eq!(report.errored, 0);
    assert!(report.stale_calls.is_empty());
    assert!(report.error_calls.is_empty());
}

#[test]
fn ipc_integrity_with_errors() {
    let log = EventLog::new(100);
    log.push(AppEvent::Ipc(IpcCall {
        id: "1".to_string(),
        command: "save".to_string(),
        timestamp: Utc::now(),
        duration_ms: Some(5),
        result: event::IpcResult::Err("permission denied".to_string()),
        arg_size_bytes: 10,
        webview_label: "main".to_string(),
    }));

    let report = victauri_core::check_ipc_integrity(&log, 5000);
    assert!(!report.healthy);
    assert_eq!(report.errored, 1);
    assert_eq!(report.error_calls.len(), 1);
    assert_eq!(report.error_calls[0].error, "permission denied");
    assert_eq!(report.error_calls[0].command, "save");
}

#[test]
fn ipc_integrity_stale_pending() {
    let log = EventLog::new(100);
    let old_timestamp = Utc::now() - chrono::Duration::seconds(10);
    log.push(AppEvent::Ipc(IpcCall {
        id: "1".to_string(),
        command: "slow_cmd".to_string(),
        timestamp: old_timestamp,
        duration_ms: None,
        result: event::IpcResult::Pending,
        arg_size_bytes: 0,
        webview_label: "main".to_string(),
    }));

    let report = victauri_core::check_ipc_integrity(&log, 5000);
    assert!(!report.healthy);
    assert_eq!(report.pending, 1);
    assert_eq!(report.stale_calls.len(), 1);
    assert_eq!(report.stale_calls[0].command, "slow_cmd");
    assert!(report.stale_calls[0].age_ms >= 9000);
}

#[test]
fn ipc_integrity_recent_pending_not_stale() {
    let log = EventLog::new(100);
    log.push(AppEvent::Ipc(IpcCall {
        id: "1".to_string(),
        command: "fast_cmd".to_string(),
        timestamp: Utc::now(),
        duration_ms: None,
        result: event::IpcResult::Pending,
        arg_size_bytes: 0,
        webview_label: "main".to_string(),
    }));

    let report = victauri_core::check_ipc_integrity(&log, 5000);
    assert!(report.healthy);
    assert_eq!(report.pending, 1);
    assert!(report.stale_calls.is_empty());
}

#[test]
fn ipc_integrity_mixed_status() {
    let log = EventLog::new(100);
    let old = Utc::now() - chrono::Duration::seconds(30);

    log.push(AppEvent::Ipc(IpcCall {
        id: "1".to_string(),
        command: "ok_cmd".to_string(),
        timestamp: Utc::now(),
        duration_ms: Some(1),
        result: event::IpcResult::Ok(serde_json::json!(null)),
        arg_size_bytes: 0,
        webview_label: "main".to_string(),
    }));
    log.push(AppEvent::Ipc(IpcCall {
        id: "2".to_string(),
        command: "stuck_cmd".to_string(),
        timestamp: old,
        duration_ms: None,
        result: event::IpcResult::Pending,
        arg_size_bytes: 0,
        webview_label: "main".to_string(),
    }));
    log.push(AppEvent::Ipc(IpcCall {
        id: "3".to_string(),
        command: "err_cmd".to_string(),
        timestamp: Utc::now(),
        duration_ms: Some(2),
        result: event::IpcResult::Err("boom".to_string()),
        arg_size_bytes: 0,
        webview_label: "main".to_string(),
    }));

    let report = victauri_core::check_ipc_integrity(&log, 5000);
    assert!(!report.healthy);
    assert_eq!(report.total_calls, 3);
    assert_eq!(report.completed, 1);
    assert_eq!(report.pending, 1);
    assert_eq!(report.errored, 1);
    assert_eq!(report.stale_calls.len(), 1);
    assert_eq!(report.error_calls.len(), 1);
}

// ── Phase 4: Intent annotations and NL resolution ──────────────────────────

#[test]
fn command_info_with_intent_fields() {
    let registry = CommandRegistry::new();
    let mut cmd = CommandInfo::new("save_settings")
        .with_description("Persist user settings")
        .with_intent("persist user preferences to storage")
        .with_category("settings");
    cmd.is_async = true;
    cmd.examples = vec![
        "save my settings".to_string(),
        "persist preferences".to_string(),
    ];
    registry.register(cmd);

    let cmd = registry.get("save_settings").unwrap();
    assert_eq!(
        cmd.intent.as_deref(),
        Some("persist user preferences to storage")
    );
    assert_eq!(cmd.category.as_deref(), Some("settings"));
    assert_eq!(cmd.examples.len(), 2);
}

#[test]
fn resolve_command_by_name() {
    let registry = CommandRegistry::new();
    registry.register(CommandInfo::new("save_file").with_description("Save a file to disk"));
    registry.register(CommandInfo::new("delete_file").with_description("Delete a file"));

    let results = registry.resolve("save file");
    assert!(!results.is_empty());
    assert_eq!(results[0].command.name, "save_file");
    assert!(results[0].score > results.get(1).map_or(0.0, |r| r.score));
}

#[test]
fn resolve_command_by_intent() {
    let registry = CommandRegistry::new();
    let mut cmd = CommandInfo::new("update_profile")
        .with_description("Update user profile")
        .with_intent("modify the current user's profile information")
        .with_category("user");
    cmd.examples = vec!["change my name".to_string()];
    registry.register(cmd);
    registry.register(
        CommandInfo::new("get_profile")
            .with_description("Fetch user profile")
            .with_intent("retrieve the current user's profile data")
            .with_category("user"),
    );

    let results = registry.resolve("modify profile");
    assert!(!results.is_empty());
    assert_eq!(results[0].command.name, "update_profile");
}

#[test]
fn resolve_exact_intent_beats_incidental_name_match() {
    // Regression: a query that is *exactly* a command's intent must win over a
    // command whose name merely contains one of the query words. Before the
    // exact-intent bonus, `resolve("increase counter")` ranked `get_counter`
    // (name contains "counter", +5.0) above `increment` (intent is literally
    // "increase counter" but name matches neither word).
    let registry = CommandRegistry::new();
    registry.register(
        CommandInfo::new("increment")
            .with_description("Increment the counter by 1 and return the new value")
            .with_intent("increase counter"),
    );
    registry.register(
        CommandInfo::new("get_counter")
            .with_description("Get the current counter value")
            .with_intent("read counter state"),
    );
    registry.register(
        CommandInfo::new("reset_counter")
            .with_description("Reset the counter to zero")
            .with_intent("reset counter state"),
    );

    let results = registry.resolve("increase counter");
    assert!(!results.is_empty());
    assert_eq!(
        results[0].command.name,
        "increment",
        "exact intent match must rank first, got {:?}",
        results
            .iter()
            .map(|r| (&r.command.name, r.score))
            .collect::<Vec<_>>()
    );
}

#[test]
fn resolve_command_by_example() {
    let registry = CommandRegistry::new();
    let mut cmd = CommandInfo::new("export_data").with_description("Export data to CSV");
    cmd.examples = vec!["download my data as csv".to_string()];
    registry.register(cmd);

    let results = registry.resolve("download my data as csv");
    assert!(!results.is_empty());
    assert_eq!(results[0].command.name, "export_data");
}

#[test]
fn resolve_command_no_match() {
    let registry = CommandRegistry::new();
    registry.register(CommandInfo::new("save").with_description("Save data"));

    let results = registry.resolve("zzz_nonexistent_zzz");
    assert!(results.is_empty());
}

#[test]
fn resolve_scores_normalized_across_query_lengths() {
    let registry = CommandRegistry::new();
    // Craft a command where many different words each independently hit the
    // description so that adding more query words inflates the raw score.
    registry.register(
        CommandInfo::new("save_user_data")
            .with_description("save the current user data to the local database")
            .with_intent("persist user data locally")
            .with_category("storage"),
    );

    let one_word = registry.resolve("save");
    let four_word = registry.resolve("save user data locally");

    assert!(!one_word.is_empty(), "one-word query must match");
    assert!(!four_word.is_empty(), "four-word query must match");
    let ratio = four_word[0].score / one_word[0].score;
    // Without normalization this ratio would be ~3-4x; with normalization it
    // should stay well below 1.8.
    assert!(
        ratio < 1.8,
        "score ratio {ratio} suggests missing normalization"
    );
}

// ── Phase 4: Semantic assertions ────────────────────────────────────────────

#[test]
fn semantic_assertion_equals() {
    let assertion = victauri_core::SemanticAssertion {
        label: "count is 5".to_string(),
        condition: victauri_core::AssertionCondition::Equals,
        expected: serde_json::json!(5),
    };

    let pass = victauri_core::evaluate_assertion(serde_json::json!(5), &assertion);
    assert!(pass.passed);
    assert!(pass.message.is_none());

    let fail = victauri_core::evaluate_assertion(serde_json::json!(3), &assertion);
    assert!(!fail.passed);
    assert!(fail.message.is_some());
}

#[test]
fn semantic_assertion_truthy_falsy() {
    let truthy = victauri_core::SemanticAssertion {
        label: "value is truthy".to_string(),
        condition: victauri_core::AssertionCondition::Truthy,
        expected: serde_json::Value::Null,
    };

    assert!(victauri_core::evaluate_assertion(serde_json::json!(true), &truthy).passed);
    assert!(victauri_core::evaluate_assertion(serde_json::json!("hello"), &truthy).passed);
    assert!(victauri_core::evaluate_assertion(serde_json::json!(42), &truthy).passed);
    assert!(!victauri_core::evaluate_assertion(serde_json::Value::Null, &truthy).passed);

    let falsy = victauri_core::SemanticAssertion {
        label: "value is falsy".to_string(),
        condition: victauri_core::AssertionCondition::Falsy,
        expected: serde_json::Value::Null,
    };

    assert!(victauri_core::evaluate_assertion(serde_json::Value::Null, &falsy).passed);
    assert!(victauri_core::evaluate_assertion(serde_json::json!(false), &falsy).passed);
    assert!(victauri_core::evaluate_assertion(serde_json::json!(0), &falsy).passed);
    assert!(!victauri_core::evaluate_assertion(serde_json::json!(1), &falsy).passed);
}

#[test]
fn semantic_assertion_contains() {
    let assertion = victauri_core::SemanticAssertion {
        label: "string contains hello".to_string(),
        condition: victauri_core::AssertionCondition::Contains,
        expected: serde_json::json!("hello"),
    };

    assert!(
        victauri_core::evaluate_assertion(serde_json::json!("say hello world"), &assertion).passed
    );
    assert!(!victauri_core::evaluate_assertion(serde_json::json!("goodbye"), &assertion).passed);
}

#[test]
fn semantic_assertion_comparisons() {
    let gt = victauri_core::SemanticAssertion {
        label: "greater than 10".to_string(),
        condition: victauri_core::AssertionCondition::GreaterThan,
        expected: serde_json::json!(10),
    };

    assert!(victauri_core::evaluate_assertion(serde_json::json!(15), &gt).passed);
    assert!(!victauri_core::evaluate_assertion(serde_json::json!(5), &gt).passed);

    let lt = victauri_core::SemanticAssertion {
        label: "less than 10".to_string(),
        condition: victauri_core::AssertionCondition::LessThan,
        expected: serde_json::json!(10),
    };

    assert!(victauri_core::evaluate_assertion(serde_json::json!(5), &lt).passed);
    assert!(!victauri_core::evaluate_assertion(serde_json::json!(15), &lt).passed);
}

#[test]
fn semantic_assertion_type_is() {
    let assertion = victauri_core::SemanticAssertion {
        label: "is a string".to_string(),
        condition: victauri_core::AssertionCondition::TypeIs,
        expected: serde_json::json!("string"),
    };

    assert!(victauri_core::evaluate_assertion(serde_json::json!("hello"), &assertion).passed);
    assert!(!victauri_core::evaluate_assertion(serde_json::json!(42), &assertion).passed);
}

#[test]
fn semantic_assertion_exists() {
    let assertion = victauri_core::SemanticAssertion {
        label: "value exists".to_string(),
        condition: victauri_core::AssertionCondition::Exists,
        expected: serde_json::Value::Null,
    };

    assert!(victauri_core::evaluate_assertion(serde_json::json!("something"), &assertion).passed);
    assert!(!victauri_core::evaluate_assertion(serde_json::Value::Null, &assertion).passed);
}

// ── Phase 5: Time-Travel (recording, checkpoints, replay) ──────────────────

#[test]
fn recorder_start_stop() {
    let recorder = EventRecorder::new(1000);
    assert!(!recorder.is_recording());

    recorder.start("session-1".to_string()).unwrap();
    assert!(recorder.is_recording());
    assert!(recorder.start("session-2".to_string()).is_err());

    let session = recorder.stop().unwrap();
    assert_eq!(session.id, "session-1");
    assert!(session.events.is_empty());
    assert!(!recorder.is_recording());
}

#[test]
fn recorder_record_events() {
    let recorder = EventRecorder::new(1000);
    recorder.start("s1".to_string()).unwrap();

    recorder.record_event(AppEvent::Ipc(IpcCall {
        id: "1".to_string(),
        command: "save".to_string(),
        timestamp: Utc::now(),
        duration_ms: Some(5),
        result: event::IpcResult::Ok(serde_json::json!("ok")),
        arg_size_bytes: 10,
        webview_label: "main".to_string(),
    }));

    recorder.record_event(AppEvent::StateChange {
        key: "user".to_string(),
        timestamp: Utc::now(),
        caused_by: Some("save".to_string()),
    });

    assert_eq!(recorder.event_count(), 2);

    let session = recorder.stop().unwrap();
    assert_eq!(session.events.len(), 2);
    assert_eq!(session.events[0].index, 0);
    assert_eq!(session.events[1].index, 1);
}

#[test]
fn recorder_checkpoints() {
    let recorder = EventRecorder::new(1000);
    assert!(
        recorder
            .checkpoint("cp1".to_string(), None, serde_json::json!({}))
            .is_err()
    );

    recorder.start("s1".to_string()).unwrap();

    recorder.record_event(AppEvent::Ipc(IpcCall {
        id: "1".to_string(),
        command: "load".to_string(),
        timestamp: Utc::now(),
        duration_ms: None,
        result: event::IpcResult::Pending,
        arg_size_bytes: 0,
        webview_label: "main".to_string(),
    }));

    recorder
        .checkpoint(
            "before-save".to_string(),
            Some("Before saving".to_string()),
            serde_json::json!({"count": 0}),
        )
        .unwrap();

    recorder.record_event(AppEvent::Ipc(IpcCall {
        id: "2".to_string(),
        command: "save".to_string(),
        timestamp: Utc::now(),
        duration_ms: Some(5),
        result: event::IpcResult::Ok(serde_json::json!("ok")),
        arg_size_bytes: 10,
        webview_label: "main".to_string(),
    }));

    recorder
        .checkpoint(
            "after-save".to_string(),
            Some("After saving".to_string()),
            serde_json::json!({"count": 1}),
        )
        .unwrap();

    assert_eq!(recorder.checkpoint_count(), 2);

    let checkpoints = recorder.get_checkpoints();
    assert_eq!(checkpoints.len(), 2);
    assert_eq!(checkpoints[0].id, "before-save");
    assert_eq!(checkpoints[1].id, "after-save");
    assert_eq!(checkpoints[0].state, serde_json::json!({"count": 0}));
    assert_eq!(checkpoints[1].state, serde_json::json!({"count": 1}));
}

#[test]
fn recorder_events_since() {
    let recorder = EventRecorder::new(1000);
    recorder.start("s1".to_string()).unwrap();

    for i in 0..5 {
        recorder.record_event(AppEvent::Ipc(IpcCall {
            id: i.to_string(),
            command: format!("cmd_{i}"),
            timestamp: Utc::now(),
            duration_ms: None,
            result: event::IpcResult::Pending,
            arg_size_bytes: 0,
            webview_label: "main".to_string(),
        }));
    }

    let since_3 = recorder.events_since(3);
    assert_eq!(since_3.len(), 2);
    assert_eq!(since_3[0].index, 3);
    assert_eq!(since_3[1].index, 4);
}

#[test]
fn recorder_events_between_checkpoints() {
    let recorder = EventRecorder::new(1000);
    recorder.start("s1".to_string()).unwrap();

    recorder.record_event(AppEvent::Ipc(IpcCall {
        id: "0".to_string(),
        command: "before".to_string(),
        timestamp: Utc::now(),
        duration_ms: None,
        result: event::IpcResult::Pending,
        arg_size_bytes: 0,
        webview_label: "main".to_string(),
    }));

    recorder
        .checkpoint("cp1".to_string(), None, serde_json::json!(null))
        .unwrap();

    recorder.record_event(AppEvent::Ipc(IpcCall {
        id: "1".to_string(),
        command: "between".to_string(),
        timestamp: Utc::now(),
        duration_ms: None,
        result: event::IpcResult::Pending,
        arg_size_bytes: 0,
        webview_label: "main".to_string(),
    }));

    recorder.record_event(AppEvent::Ipc(IpcCall {
        id: "2".to_string(),
        command: "between2".to_string(),
        timestamp: Utc::now(),
        duration_ms: None,
        result: event::IpcResult::Pending,
        arg_size_bytes: 0,
        webview_label: "main".to_string(),
    }));

    recorder
        .checkpoint("cp2".to_string(), None, serde_json::json!(null))
        .unwrap();

    let between = recorder.events_between_checkpoints("cp1", "cp2").unwrap();
    assert_eq!(between.len(), 2);

    assert!(
        recorder
            .events_between_checkpoints("cp1", "nonexistent")
            .is_err()
    );
}

#[test]
fn recorder_ipc_replay_sequence() {
    let recorder = EventRecorder::new(1000);
    recorder.start("s1".to_string()).unwrap();

    recorder.record_event(AppEvent::Ipc(IpcCall {
        id: "1".to_string(),
        command: "save".to_string(),
        timestamp: Utc::now(),
        duration_ms: Some(5),
        result: event::IpcResult::Ok(serde_json::json!("ok")),
        arg_size_bytes: 10,
        webview_label: "main".to_string(),
    }));

    recorder.record_event(AppEvent::StateChange {
        key: "user".to_string(),
        timestamp: Utc::now(),
        caused_by: None,
    });

    recorder.record_event(AppEvent::Ipc(IpcCall {
        id: "2".to_string(),
        command: "load".to_string(),
        timestamp: Utc::now(),
        duration_ms: Some(3),
        result: event::IpcResult::Ok(serde_json::json!("data")),
        arg_size_bytes: 0,
        webview_label: "main".to_string(),
    }));

    let replay = recorder.ipc_replay_sequence();
    assert_eq!(replay.len(), 2);
    assert_eq!(replay[0].command, "save");
    assert_eq!(replay[1].command, "load");
}

#[test]
fn recorder_not_recording_returns_empty() {
    let recorder = EventRecorder::new(1000);
    assert_eq!(recorder.event_count(), 0);
    assert_eq!(recorder.checkpoint_count(), 0);
    assert!(recorder.events_since(0).is_empty());
    assert!(recorder.ipc_replay_sequence().is_empty());
    assert!(recorder.stop().is_none());
}

#[test]
fn recorder_export_does_not_stop_recording() {
    let recorder = EventRecorder::new(1000);
    recorder.start("s1".to_string()).unwrap();
    recorder.record_event(AppEvent::StateChange {
        key: "k".to_string(),
        timestamp: Utc::now(),
        caused_by: None,
    });

    let exported = recorder.export();
    assert!(exported.is_some());
    let session = exported.unwrap();
    assert_eq!(session.id, "s1");
    assert_eq!(session.events.len(), 1);

    assert!(
        recorder.is_recording(),
        "export must not stop the recording"
    );
    assert_eq!(recorder.event_count(), 1);

    recorder.record_event(AppEvent::StateChange {
        key: "k2".to_string(),
        timestamp: Utc::now(),
        caused_by: None,
    });
    assert_eq!(recorder.event_count(), 2);
}

#[test]
fn recorder_export_returns_none_when_not_recording() {
    let recorder = EventRecorder::new(1000);
    assert!(recorder.export().is_none());
}

#[test]
fn recorder_import_replaces_active_recording() {
    let recorder = EventRecorder::new(1000);
    recorder.start("original".to_string()).unwrap();
    recorder.record_event(AppEvent::StateChange {
        key: "k".to_string(),
        timestamp: Utc::now(),
        caused_by: None,
    });

    let session = victauri_core::RecordedSession {
        id: "imported".to_string(),
        started_at: Utc::now(),
        events: vec![
            victauri_core::RecordedEvent {
                index: 0,
                timestamp: Utc::now(),
                event: AppEvent::StateChange {
                    key: "a".to_string(),
                    timestamp: Utc::now(),
                    caused_by: None,
                },
            },
            victauri_core::RecordedEvent {
                index: 1,
                timestamp: Utc::now(),
                event: AppEvent::StateChange {
                    key: "b".to_string(),
                    timestamp: Utc::now(),
                    caused_by: None,
                },
            },
        ],
        checkpoints: vec![],
    };

    recorder.import(session);
    assert!(recorder.is_recording());
    assert_eq!(recorder.event_count(), 2);

    let stopped = recorder.stop().unwrap();
    assert_eq!(stopped.id, "imported");
    assert_eq!(stopped.events.len(), 2);
}

#[test]
fn recorder_import_when_not_recording() {
    let recorder = EventRecorder::new(1000);
    assert!(!recorder.is_recording());

    let session = victauri_core::RecordedSession {
        id: "fresh".to_string(),
        started_at: Utc::now(),
        events: vec![],
        checkpoints: vec![],
    };

    recorder.import(session);
    assert!(recorder.is_recording());
    assert_eq!(recorder.event_count(), 0);
}

#[test]
fn truthy_falsy_are_never_both_true() {
    use victauri_core::verification;
    let truthy = verification::SemanticAssertion {
        label: "t".to_string(),
        condition: verification::AssertionCondition::Truthy,
        expected: serde_json::Value::Null,
    };
    let falsy = verification::SemanticAssertion {
        label: "f".to_string(),
        condition: verification::AssertionCondition::Falsy,
        expected: serde_json::Value::Null,
    };
    let test_values = vec![
        serde_json::json!(null),
        serde_json::json!(true),
        serde_json::json!(false),
        serde_json::json!(0),
        serde_json::json!(1),
        serde_json::json!(-1),
        serde_json::json!(0.0),
        serde_json::json!(42),
        serde_json::json!(""),
        serde_json::json!("hello"),
        serde_json::json!([]),
        serde_json::json!([1]),
        serde_json::json!({}),
        serde_json::json!({"k": "v"}),
    ];
    for val in test_values {
        let is_truthy = verification::evaluate_assertion(val.clone(), &truthy).passed;
        let is_falsy = verification::evaluate_assertion(val.clone(), &falsy).passed;
        assert!(
            is_truthy != is_falsy,
            "value {val} is both truthy={is_truthy} and falsy={is_falsy} — contradiction"
        );
    }
}

// ── Adversarial Tests ─────────────────────────────────────────────────────

mod adversarial {
    use super::*;
    use std::thread;

    fn make_ipc(id: &str, cmd: &str) -> AppEvent {
        AppEvent::Ipc(IpcCall {
            id: id.to_string(),
            command: cmd.to_string(),
            timestamp: Utc::now(),
            duration_ms: Some(1),
            result: event::IpcResult::Ok(serde_json::json!("ok")),
            arg_size_bytes: 0,
            webview_label: "main".to_string(),
        })
    }

    // ── Mutex poisoning recovery ────────────────────────────────────────

    #[test]
    fn event_log_survives_poisoned_mutex() {
        let log = EventLog::new(10);
        let log2 = log.clone();
        let _ = thread::spawn(move || {
            let _guard = log2.snapshot();
            panic!("intentional panic while holding lock");
        })
        .join();
        // After poisoning, operations should still work
        log.push(make_ipc("1", "after_poison"));
        assert_eq!(log.len(), 1);
        assert!(!log.is_empty());
        let snap = log.snapshot();
        assert_eq!(snap.len(), 1);
    }

    #[test]
    fn recorder_survives_poisoned_mutex() {
        let rec = EventRecorder::new(100);
        let rec2 = rec.clone();
        let _ = thread::spawn(move || {
            let _guard = rec2.event_count();
            panic!("intentional panic while holding lock");
        })
        .join();
        // After poisoning, operations should still work via recovery
        assert!(!rec.is_recording());
        assert_eq!(rec.event_count(), 0);
        rec.start("after-poison".to_string()).unwrap();
        rec.record_event(make_ipc("1", "test"));
        assert_eq!(rec.event_count(), 1);
    }

    #[test]
    fn registry_survives_poisoned_rwlock() {
        let reg = CommandRegistry::new();
        let reg2 = reg.clone();
        let _ = thread::spawn(move || {
            let _list = reg2.list();
            panic!("intentional panic while holding read lock");
        })
        .join();
        reg.register(
            registry::CommandInfo::new("after_poison").with_description("works after poisoning"),
        );
        assert_eq!(reg.count(), 1);
    }

    // ── Concurrent access ───────────────────────────────────────────────

    #[test]
    fn event_log_concurrent_push_and_read() {
        let log = Arc::new(EventLog::new(1000));
        let mut handles = Vec::new();
        for i in 0..10 {
            let log = Arc::clone(&log);
            handles.push(thread::spawn(move || {
                for j in 0..100 {
                    log.push(make_ipc(&format!("{i}-{j}"), "concurrent"));
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(log.len(), 1000);
    }

    #[test]
    fn registry_concurrent_register_and_search() {
        let reg = Arc::new(CommandRegistry::new());
        let mut handles = Vec::new();
        for i in 0..10 {
            let reg = Arc::clone(&reg);
            handles.push(thread::spawn(move || {
                for j in 0..10 {
                    reg.register(
                        registry::CommandInfo::new(format!("cmd_{i}_{j}"))
                            .with_description(format!("Command {i}-{j}")),
                    );
                }
            }));
        }
        for i in 0..5 {
            let reg = Arc::clone(&reg);
            handles.push(thread::spawn(move || {
                for _ in 0..20 {
                    let _ = reg.search(&format!("cmd_{i}"));
                    let _ = reg.resolve(&format!("cmd {i}"));
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(reg.count(), 100);
    }

    // ── Ring buffer edge cases ──────────────────────────────────────────

    #[test]
    fn event_log_capacity_one() {
        let log = EventLog::new(1);
        log.push(make_ipc("1", "first"));
        log.push(make_ipc("2", "second"));
        assert_eq!(log.len(), 1);
        let snap = log.snapshot();
        match &snap[0] {
            AppEvent::Ipc(call) => assert_eq!(call.id, "2"),
            _ => panic!("expected IPC event"),
        }
    }

    #[test]
    fn recorder_ring_buffer_wraps_correctly() {
        let rec = EventRecorder::new(3);
        rec.start("wrap-test".to_string()).unwrap();
        for i in 0..5 {
            rec.record_event(make_ipc(&i.to_string(), &format!("cmd_{i}")));
        }
        assert_eq!(rec.event_count(), 3);
        let session = rec.stop().unwrap();
        assert_eq!(session.events.len(), 3);
        // Should have events 2, 3, 4 (oldest evicted)
        assert_eq!(session.events[0].index, 2);
        assert_eq!(session.events[2].index, 4);
    }

    #[test]
    fn recorder_event_index_stays_monotonic_after_wrap() {
        let rec = EventRecorder::new(2);
        rec.start("mono-test".to_string()).unwrap();
        for i in 0..10 {
            rec.record_event(make_ipc(&i.to_string(), "wrap"));
        }
        let session = rec.stop().unwrap();
        for pair in session.events.windows(2) {
            assert!(pair[1].index > pair[0].index);
        }
    }

    // ── Verification edge cases ─────────────────────────────────────────

    #[test]
    fn verify_both_empty_objects_pass() {
        let result = verify_state(serde_json::json!({}), serde_json::json!({}));
        assert!(result.passed);
    }

    #[test]
    fn verify_deeply_nested_divergence() {
        let frontend = serde_json::json!({"a": {"b": {"c": {"d": 1}}}});
        let backend = serde_json::json!({"a": {"b": {"c": {"d": 2}}}});
        let result = verify_state(frontend, backend);
        assert!(!result.passed);
        assert_eq!(result.divergences[0].path, "a.b.c.d");
    }

    #[test]
    fn verify_type_mismatch_string_vs_number() {
        let frontend = serde_json::json!({"count": "5"});
        let backend = serde_json::json!({"count": 5});
        let result = verify_state(frontend, backend);
        assert!(!result.passed);
        assert_eq!(
            result.divergences[0].severity,
            types::DivergenceSeverity::Error
        );
    }

    #[test]
    fn verify_null_vs_missing_key() {
        let frontend = serde_json::json!({"a": null});
        let backend = serde_json::json!({});
        let result = verify_state(frontend, backend);
        assert!(!result.passed);
    }

    #[test]
    fn verify_empty_arrays_pass() {
        let result = verify_state(serde_json::json!({"a": []}), serde_json::json!({"a": []}));
        assert!(result.passed);
    }

    #[test]
    fn verify_array_length_mismatch() {
        let frontend = serde_json::json!({"arr": [1, 2, 3]});
        let backend = serde_json::json!({"arr": [1, 2]});
        let result = verify_state(frontend, backend);
        assert!(!result.passed);
        assert!(result.divergences.iter().any(|d| d.path.contains("[2]")));
    }

    // ── Ghost command edge cases ────────────────────────────────────────

    #[test]
    fn ghost_commands_empty_both_sides() {
        let registry = CommandRegistry::new();
        let report = detect_ghost_commands(&[], &registry);
        assert!(report.frontend_only.is_empty());
        assert!(report.registry_only.is_empty());
        assert_eq!(report.total_frontend_commands, 0);
        assert_eq!(report.total_registry_commands, 0);
    }

    #[test]
    fn ghost_commands_perfect_match() {
        let registry = CommandRegistry::new();
        registry.register(registry::CommandInfo::new("get_settings"));
        let frontend = vec!["get_settings".to_string()];
        let report = detect_ghost_commands(&frontend, &registry);
        assert!(report.frontend_only.is_empty());
        assert!(report.registry_only.is_empty());
    }

    #[test]
    fn ghost_commands_deduplicates_frontend() {
        let registry = CommandRegistry::new();
        registry.register(registry::CommandInfo::new("get_settings"));
        let frontend = vec![
            "get_settings".to_string(),
            "get_settings".to_string(),
            "get_settings".to_string(),
        ];
        let report = detect_ghost_commands(&frontend, &registry);
        assert!(report.frontend_only.is_empty());
        assert!(report.registry_only.is_empty());
        assert_eq!(report.total_frontend_commands, 1);
    }

    // ── Assertion edge cases ────────────────────────────────────────────

    #[test]
    fn assertion_truthy_edge_cases() {
        let truthy = verification::SemanticAssertion {
            label: "t".to_string(),
            condition: verification::AssertionCondition::Truthy,
            expected: serde_json::json!(null),
        };
        // 0 is falsy (JS semantics — Victauri evaluates JS expressions)
        assert!(!verification::evaluate_assertion(serde_json::json!(0), &truthy).passed);
        // Non-zero numbers are truthy
        assert!(verification::evaluate_assertion(serde_json::json!(42), &truthy).passed);
        // Empty string is falsy
        assert!(!verification::evaluate_assertion(serde_json::json!(""), &truthy).passed);
        // Non-empty string is truthy
        assert!(verification::evaluate_assertion(serde_json::json!("x"), &truthy).passed);
        // null is falsy
        assert!(!verification::evaluate_assertion(serde_json::Value::Null, &truthy).passed);
        // false is falsy
        assert!(!verification::evaluate_assertion(serde_json::json!(false), &truthy).passed);
    }

    #[test]
    fn assertion_contains_in_array() {
        let assertion = verification::SemanticAssertion {
            label: "arr".to_string(),
            condition: verification::AssertionCondition::Contains,
            expected: serde_json::json!(2),
        };
        assert!(verification::evaluate_assertion(serde_json::json!([1, 2, 3]), &assertion).passed);
        assert!(!verification::evaluate_assertion(serde_json::json!([1, 3, 5]), &assertion).passed);
    }

    // ── IPC integrity edge cases ────────────────────────────────────────

    #[test]
    fn ipc_integrity_empty_log() {
        let log = EventLog::new(100);
        let report = check_ipc_integrity(&log, 5000);
        assert!(report.healthy);
        assert_eq!(report.total_calls, 0);
    }

    #[test]
    fn ipc_integrity_detects_errors() {
        let log = EventLog::new(100);
        log.push(AppEvent::Ipc(IpcCall {
            id: "err1".to_string(),
            command: "broken_cmd".to_string(),
            timestamp: Utc::now(),
            duration_ms: None,
            result: event::IpcResult::Err("something failed".to_string()),
            arg_size_bytes: 0,
            webview_label: "main".to_string(),
        }));
        let report = check_ipc_integrity(&log, 5000);
        assert!(!report.healthy);
        assert_eq!(report.errored, 1);
        assert_eq!(report.error_calls[0].command, "broken_cmd");
    }

    // ── Recording checkpoint limits ─────────────────────────────────────

    #[test]
    fn recorder_double_start_returns_false() {
        let rec = EventRecorder::new(100);
        rec.start("first".to_string()).unwrap();
        assert!(rec.start("second".to_string()).is_err());
        let session = rec.stop().unwrap();
        assert_eq!(session.id, "first");
    }

    #[test]
    fn recorder_checkpoint_without_recording_returns_false() {
        let rec = EventRecorder::new(100);
        assert!(
            rec.checkpoint("cp1".to_string(), None, serde_json::json!({}))
                .is_err()
        );
    }

    #[test]
    fn recorder_default_impl() {
        let rec = EventRecorder::default();
        assert!(!rec.is_recording());
        rec.start("default-test".to_string()).unwrap();
        for i in 0..100 {
            rec.record_event(make_ipc(&i.to_string(), "default"));
        }
        assert_eq!(rec.event_count(), 100);
        let _ = rec.stop();
    }

    // ── Registry sort stability with equal scores ───────────────────────

    #[test]
    fn registry_resolve_with_no_match() {
        let reg = CommandRegistry::new();
        reg.register(registry::CommandInfo::new("alpha"));
        let results = reg.resolve("zzz_nonexistent_query");
        assert!(results.is_empty());
    }

    #[test]
    fn registry_resolve_empty_query() {
        let reg = CommandRegistry::new();
        reg.register(registry::CommandInfo::new("alpha"));
        let results = reg.resolve("");
        assert!(results.is_empty());
    }

    // ── Recording concurrency ───────────────────────────────────────────

    #[test]
    fn recorder_concurrent_events() {
        let rec = Arc::new(EventRecorder::new(10_000));
        rec.start("concurrent".to_string()).unwrap();
        let mut handles = Vec::new();
        for i in 0..10 {
            let r = Arc::clone(&rec);
            handles.push(thread::spawn(move || {
                for j in 0..100 {
                    r.record_event(make_ipc(&format!("{i}-{j}"), &format!("thread_{i}")));
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(rec.event_count(), 1000);
        let session = rec.stop().unwrap();
        assert_eq!(session.events.len(), 1000);
    }

    #[test]
    fn recorder_concurrent_checkpoints() {
        let rec = Arc::new(EventRecorder::new(10_000));
        rec.start("cp-concurrent".to_string()).unwrap();
        let mut handles = Vec::new();
        for i in 0..5 {
            let r = Arc::clone(&rec);
            handles.push(thread::spawn(move || {
                for j in 0..10 {
                    r.checkpoint(
                        format!("cp-{i}-{j}"),
                        Some(format!("Thread {i} checkpoint {j}")),
                        serde_json::json!({"thread": i, "seq": j}),
                    )
                    .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(rec.checkpoint_count(), 50);
    }

    #[test]
    fn recorder_stop_while_recording() {
        let rec = Arc::new(EventRecorder::new(10_000));
        rec.start("stop-test".to_string()).unwrap();

        let r = Arc::clone(&rec);
        let writer = thread::spawn(move || {
            for i in 0..1000 {
                r.record_event(make_ipc(&i.to_string(), "spam"));
            }
        });

        // Stop before all events are written
        std::thread::sleep(std::time::Duration::from_millis(1));
        let session = rec.stop();
        writer.join().unwrap();

        // Session should have been captured (may not have all 1000 events)
        assert!(session.is_some());
        let s = session.unwrap();
        assert!(s.events.len() <= 1000);
    }

    // ── Session serialization round-trip ────────────────────────────────

    #[test]
    fn recorded_session_serde_roundtrip() {
        let rec = EventRecorder::new(1000);
        rec.start("serde-test".to_string()).unwrap();
        rec.record_event(make_ipc("1", "save"));
        rec.record_event(make_ipc("2", "load"));
        rec.checkpoint(
            "cp1".to_string(),
            Some("mid".to_string()),
            serde_json::json!({"count": 1}),
        )
        .unwrap();
        let session = rec.stop().unwrap();

        let json = serde_json::to_string(&session).unwrap();
        let deserialized: victauri_core::RecordedSession = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, "serde-test");
        assert_eq!(deserialized.events.len(), 2);
        assert_eq!(deserialized.checkpoints.len(), 1);
        assert_eq!(deserialized.checkpoints[0].id, "cp1");
    }

    // ── Event log pagination (since filter) ─────────────────────────────

    #[test]
    fn event_log_since_filters_correctly() {
        let log = EventLog::new(100);
        let t1 = Utc::now();
        std::thread::sleep(std::time::Duration::from_millis(10));
        log.push(AppEvent::Ipc(IpcCall {
            id: "1".to_string(),
            command: "old".to_string(),
            timestamp: t1,
            duration_ms: None,
            result: event::IpcResult::Pending,
            arg_size_bytes: 0,
            webview_label: "main".to_string(),
        }));
        let t2 = Utc::now();
        std::thread::sleep(std::time::Duration::from_millis(10));
        log.push(AppEvent::Ipc(IpcCall {
            id: "2".to_string(),
            command: "new".to_string(),
            timestamp: Utc::now(),
            duration_ms: None,
            result: event::IpcResult::Pending,
            arg_size_bytes: 0,
            webview_label: "main".to_string(),
        }));

        let since = log.since(t2);
        assert_eq!(since.len(), 1);
    }

    #[test]
    fn event_log_since_all_event_types() {
        let log = EventLog::new(100);
        let past = Utc::now() - chrono::Duration::seconds(10);
        let future = Utc::now() + chrono::Duration::seconds(10);

        log.push(AppEvent::Ipc(IpcCall {
            id: "1".to_string(),
            command: "test".to_string(),
            timestamp: past,
            duration_ms: None,
            result: event::IpcResult::Pending,
            arg_size_bytes: 0,
            webview_label: "main".to_string(),
        }));
        log.push(AppEvent::StateChange {
            key: "k".to_string(),
            timestamp: future,
            caused_by: None,
        });
        log.push(AppEvent::DomMutation {
            webview_label: "main".to_string(),
            timestamp: past,
            mutation_count: 1,
        });
        log.push(AppEvent::WindowEvent {
            label: "main".to_string(),
            event: "focus".to_string(),
            timestamp: future,
        });

        let since = log.since(Utc::now());
        assert_eq!(since.len(), 2); // only the future ones
    }

    // ── Snapshot pagination ────────────────────────────────────────────

    #[test]
    fn event_log_snapshot_range() {
        let log = EventLog::new(100);
        for i in 0..10 {
            log.push(make_ipc(&i.to_string(), &format!("cmd_{i}")));
        }
        let page = log.snapshot_range(3, 4);
        assert_eq!(page.len(), 4);
        match &page[0] {
            AppEvent::Ipc(call) => assert_eq!(call.id, "3"),
            _ => panic!("expected IPC"),
        }
        match &page[3] {
            AppEvent::Ipc(call) => assert_eq!(call.id, "6"),
            _ => panic!("expected IPC"),
        }
    }

    #[test]
    fn event_log_snapshot_range_past_end() {
        let log = EventLog::new(100);
        for i in 0..5 {
            log.push(make_ipc(&i.to_string(), "cmd"));
        }
        let page = log.snapshot_range(3, 100);
        assert_eq!(page.len(), 2);
    }

    #[test]
    fn event_log_snapshot_range_empty() {
        let log = EventLog::new(100);
        let page = log.snapshot_range(0, 10);
        assert!(page.is_empty());
    }

    // ── IPC calls filtered by time ─────────────────────────────────────

    #[test]
    fn event_log_ipc_calls_since() {
        let log = EventLog::new(100);
        let past = Utc::now() - chrono::Duration::seconds(10);

        log.push(AppEvent::Ipc(IpcCall {
            id: "old".to_string(),
            command: "old_cmd".to_string(),
            timestamp: past,
            duration_ms: None,
            result: event::IpcResult::Pending,
            arg_size_bytes: 0,
            webview_label: "main".to_string(),
        }));
        log.push(AppEvent::StateChange {
            key: "k".to_string(),
            timestamp: Utc::now(),
            caused_by: None,
        });
        log.push(AppEvent::Ipc(IpcCall {
            id: "new".to_string(),
            command: "new_cmd".to_string(),
            timestamp: Utc::now(),
            duration_ms: Some(1),
            result: event::IpcResult::Ok(serde_json::json!("ok")),
            arg_size_bytes: 0,
            webview_label: "main".to_string(),
        }));

        let calls = log.ipc_calls_since(Utc::now() - chrono::Duration::seconds(1));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].command, "new_cmd");
    }
}

// ── Additional EventLog tests ─────────────────────────────────────────────

fn make_ipc_at(id: &str, cmd: &str, ts: DateTime<Utc>) -> AppEvent {
    AppEvent::Ipc(IpcCall {
        id: id.to_string(),
        command: cmd.to_string(),
        timestamp: ts,
        duration_ms: Some(1),
        result: event::IpcResult::Ok(serde_json::json!("ok")),
        arg_size_bytes: 0,
        webview_label: "main".to_string(),
    })
}

fn make_ipc_simple(id: &str, cmd: &str) -> AppEvent {
    AppEvent::Ipc(IpcCall {
        id: id.to_string(),
        command: cmd.to_string(),
        timestamp: Utc::now(),
        duration_ms: Some(1),
        result: event::IpcResult::Ok(serde_json::json!("ok")),
        arg_size_bytes: 0,
        webview_label: "main".to_string(),
    })
}

#[test]
fn event_log_snapshot_range_offset_zero_full_limit() {
    let log = EventLog::new(100);
    for i in 0..5 {
        log.push(make_ipc_simple(&i.to_string(), &format!("cmd_{i}")));
    }

    let page = log.snapshot_range(0, 5);
    assert_eq!(page.len(), 5);
    match &page[0] {
        AppEvent::Ipc(call) => assert_eq!(call.id, "0"),
        _ => panic!("expected IPC"),
    }
    match &page[4] {
        AppEvent::Ipc(call) => assert_eq!(call.id, "4"),
        _ => panic!("expected IPC"),
    }
}

#[test]
fn event_log_snapshot_range_offset_beyond_length() {
    let log = EventLog::new(100);
    for i in 0..5 {
        log.push(make_ipc_simple(&i.to_string(), "cmd"));
    }

    let page = log.snapshot_range(10, 5);
    assert!(page.is_empty());

    let page = log.snapshot_range(5, 5);
    assert!(page.is_empty());

    let page = log.snapshot_range(100, 100);
    assert!(page.is_empty());
}

#[test]
fn event_log_snapshot_range_limit_zero() {
    let log = EventLog::new(100);
    for i in 0..5 {
        log.push(make_ipc_simple(&i.to_string(), "cmd"));
    }

    let page = log.snapshot_range(0, 0);
    assert!(page.is_empty());

    let page = log.snapshot_range(2, 0);
    assert!(page.is_empty());
}

#[test]
fn event_log_snapshot_range_limit_one() {
    let log = EventLog::new(100);
    for i in 0..5 {
        log.push(make_ipc_simple(&i.to_string(), &format!("cmd_{i}")));
    }

    let page = log.snapshot_range(2, 1);
    assert_eq!(page.len(), 1);
    match &page[0] {
        AppEvent::Ipc(call) => assert_eq!(call.id, "2"),
        _ => panic!("expected IPC"),
    }
}

#[test]
fn event_log_snapshot_range_limit_exceeds_remaining() {
    let log = EventLog::new(100);
    for i in 0..5 {
        log.push(make_ipc_simple(&i.to_string(), &format!("cmd_{i}")));
    }

    // offset 3, limit 100 => only 2 events remain (indices 3 and 4)
    let page = log.snapshot_range(3, 100);
    assert_eq!(page.len(), 2);
    match &page[0] {
        AppEvent::Ipc(call) => assert_eq!(call.id, "3"),
        _ => panic!("expected IPC"),
    }
    match &page[1] {
        AppEvent::Ipc(call) => assert_eq!(call.id, "4"),
        _ => panic!("expected IPC"),
    }
}

#[test]
fn event_log_since_with_varied_timestamps() {
    let log = EventLog::new(100);
    let t_old = Utc::now() - chrono::Duration::seconds(60);
    let t_mid = Utc::now() - chrono::Duration::seconds(30);
    let t_new = Utc::now() - chrono::Duration::seconds(5);

    log.push(make_ipc_at("1", "old_cmd", t_old));
    log.push(make_ipc_at("2", "mid_cmd", t_mid));
    log.push(make_ipc_at("3", "new_cmd", t_new));

    // Since a time before all events => get all
    let all = log.since(t_old - chrono::Duration::seconds(1));
    assert_eq!(all.len(), 3);

    // Since mid => get mid and new
    let recent = log.since(t_mid);
    assert_eq!(recent.len(), 2);
    match &recent[0] {
        AppEvent::Ipc(call) => assert_eq!(call.command, "mid_cmd"),
        _ => panic!("expected IPC"),
    }
    match &recent[1] {
        AppEvent::Ipc(call) => assert_eq!(call.command, "new_cmd"),
        _ => panic!("expected IPC"),
    }

    // Since a time after all events => get none
    let none = log.since(Utc::now() + chrono::Duration::seconds(10));
    assert!(none.is_empty());
}

#[test]
fn event_log_since_filters_all_event_types() {
    let log = EventLog::new(100);
    let t_old = Utc::now() - chrono::Duration::seconds(60);
    let t_new = Utc::now() + chrono::Duration::seconds(60);
    let cutoff = Utc::now();

    log.push(make_ipc_at("1", "old_ipc", t_old));
    log.push(AppEvent::StateChange {
        key: "old_state".to_string(),
        timestamp: t_old,
        caused_by: None,
    });
    log.push(AppEvent::DomMutation {
        webview_label: "main".to_string(),
        timestamp: t_old,
        mutation_count: 5,
    });
    log.push(AppEvent::WindowEvent {
        label: "main".to_string(),
        event: "old_focus".to_string(),
        timestamp: t_old,
    });

    log.push(make_ipc_at("2", "new_ipc", t_new));
    log.push(AppEvent::StateChange {
        key: "new_state".to_string(),
        timestamp: t_new,
        caused_by: None,
    });
    log.push(AppEvent::DomMutation {
        webview_label: "main".to_string(),
        timestamp: t_new,
        mutation_count: 10,
    });
    log.push(AppEvent::WindowEvent {
        label: "main".to_string(),
        event: "new_focus".to_string(),
        timestamp: t_new,
    });

    let recent = log.since(cutoff);
    assert_eq!(recent.len(), 4);

    // Verify each type is represented
    let has_ipc = recent.iter().any(|e| matches!(e, AppEvent::Ipc(_)));
    let has_state = recent
        .iter()
        .any(|e| matches!(e, AppEvent::StateChange { .. }));
    let has_dom = recent
        .iter()
        .any(|e| matches!(e, AppEvent::DomMutation { .. }));
    let has_window = recent
        .iter()
        .any(|e| matches!(e, AppEvent::WindowEvent { .. }));
    assert!(has_ipc);
    assert!(has_state);
    assert!(has_dom);
    assert!(has_window);
}

#[test]
fn event_log_ipc_calls_returns_only_ipc_events() {
    let log = EventLog::new(100);

    log.push(make_ipc_simple("1", "save"));
    log.push(AppEvent::StateChange {
        key: "data".to_string(),
        timestamp: Utc::now(),
        caused_by: Some("save".to_string()),
    });
    log.push(AppEvent::DomMutation {
        webview_label: "main".to_string(),
        timestamp: Utc::now(),
        mutation_count: 3,
    });
    log.push(AppEvent::WindowEvent {
        label: "main".to_string(),
        event: "resize".to_string(),
        timestamp: Utc::now(),
    });
    log.push(make_ipc_simple("2", "load"));

    assert_eq!(log.len(), 5);

    let ipc = log.ipc_calls();
    assert_eq!(ipc.len(), 2);
    assert_eq!(ipc[0].command, "save");
    assert_eq!(ipc[1].command, "load");
}

#[test]
fn event_log_ipc_calls_empty_when_no_ipc_events() {
    let log = EventLog::new(100);

    log.push(AppEvent::StateChange {
        key: "data".to_string(),
        timestamp: Utc::now(),
        caused_by: None,
    });
    log.push(AppEvent::DomMutation {
        webview_label: "main".to_string(),
        timestamp: Utc::now(),
        mutation_count: 1,
    });
    log.push(AppEvent::WindowEvent {
        label: "main".to_string(),
        event: "focus".to_string(),
        timestamp: Utc::now(),
    });

    assert_eq!(log.len(), 3);
    assert!(log.ipc_calls().is_empty());
}

#[test]
fn event_log_ipc_calls_since_filters_by_timestamp() {
    let log = EventLog::new(100);
    let t_old = Utc::now() - chrono::Duration::seconds(60);
    let t_new = Utc::now() - chrono::Duration::seconds(1);
    let cutoff = Utc::now() - chrono::Duration::seconds(30);

    log.push(make_ipc_at("1", "old_cmd", t_old));
    log.push(AppEvent::StateChange {
        key: "k".to_string(),
        timestamp: t_new,
        caused_by: None,
    });
    log.push(make_ipc_at("2", "new_cmd", t_new));

    let calls = log.ipc_calls_since(cutoff);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].command, "new_cmd");
}

#[test]
fn event_log_ipc_calls_since_excludes_non_ipc() {
    let log = EventLog::new(100);
    let ts = Utc::now();

    // Add non-IPC events with recent timestamps
    log.push(AppEvent::StateChange {
        key: "k".to_string(),
        timestamp: ts,
        caused_by: None,
    });
    log.push(AppEvent::WindowEvent {
        label: "main".to_string(),
        event: "focus".to_string(),
        timestamp: ts,
    });

    let calls = log.ipc_calls_since(ts - chrono::Duration::seconds(1));
    assert!(calls.is_empty());
}

#[test]
fn event_log_is_empty_and_len_on_empty_log() {
    let log = EventLog::new(100);
    assert!(log.is_empty());
    assert_eq!(log.len(), 0);
}

#[test]
fn event_log_is_empty_and_len_after_pushes() {
    let log = EventLog::new(100);
    log.push(make_ipc_simple("1", "cmd1"));
    assert!(!log.is_empty());
    assert_eq!(log.len(), 1);

    log.push(make_ipc_simple("2", "cmd2"));
    assert!(!log.is_empty());
    assert_eq!(log.len(), 2);
}

#[test]
fn event_log_clear_empties_all_events() {
    let log = EventLog::new(100);
    for i in 0..10 {
        log.push(make_ipc_simple(&i.to_string(), "cmd"));
    }
    assert_eq!(log.len(), 10);
    assert!(!log.is_empty());

    log.clear();
    assert_eq!(log.len(), 0);
    assert!(log.is_empty());
    assert!(log.snapshot().is_empty());
    assert!(log.ipc_calls().is_empty());
}

#[test]
fn event_log_clear_then_push_works() {
    let log = EventLog::new(100);
    log.push(make_ipc_simple("1", "before_clear"));
    log.clear();
    log.push(make_ipc_simple("2", "after_clear"));

    assert_eq!(log.len(), 1);
    let calls = log.ipc_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].command, "after_clear");
}

#[test]
fn event_log_ring_buffer_eviction_preserves_newest() {
    let log = EventLog::new(5);

    for i in 0..20 {
        log.push(make_ipc_simple(&i.to_string(), &format!("cmd_{i}")));
    }

    assert_eq!(log.len(), 5);
    let calls = log.ipc_calls();
    assert_eq!(calls.len(), 5);
    // Should have the last 5 (indices 15-19)
    assert_eq!(calls[0].command, "cmd_15");
    assert_eq!(calls[1].command, "cmd_16");
    assert_eq!(calls[2].command, "cmd_17");
    assert_eq!(calls[3].command, "cmd_18");
    assert_eq!(calls[4].command, "cmd_19");
}

#[test]
fn event_log_ring_buffer_eviction_at_exact_capacity() {
    let log = EventLog::new(3);

    // Push exactly capacity events
    for i in 0..3 {
        log.push(make_ipc_simple(&i.to_string(), &format!("cmd_{i}")));
    }
    assert_eq!(log.len(), 3);

    // Push one more to trigger eviction
    log.push(make_ipc_simple("3", "cmd_3"));
    assert_eq!(log.len(), 3);

    let calls = log.ipc_calls();
    assert_eq!(calls[0].command, "cmd_1");
    assert_eq!(calls[2].command, "cmd_3");
}

#[test]
fn event_log_ring_buffer_eviction_mixed_event_types() {
    let log = EventLog::new(3);

    log.push(make_ipc_simple("1", "ipc_1"));
    log.push(AppEvent::StateChange {
        key: "state_1".to_string(),
        timestamp: Utc::now(),
        caused_by: None,
    });
    log.push(AppEvent::DomMutation {
        webview_label: "main".to_string(),
        timestamp: Utc::now(),
        mutation_count: 1,
    });

    // At capacity. Push one more IPC to evict ipc_1.
    log.push(make_ipc_simple("2", "ipc_2"));
    assert_eq!(log.len(), 3);

    // ipc_1 should be evicted; ipc_2 should remain
    let ipc = log.ipc_calls();
    assert_eq!(ipc.len(), 1);
    assert_eq!(ipc[0].command, "ipc_2");
}

// ── Additional EventRecorder tests ────────────────────────────────────────

#[test]
fn recorder_events_between_checkpoints_with_multiple_events() {
    let recorder = EventRecorder::new(1000);
    recorder.start("s1".to_string()).unwrap();

    // Events before first checkpoint
    recorder.record_event(make_ipc_simple("0", "before_all"));
    recorder.record_event(make_ipc_simple("1", "before_all_2"));

    recorder
        .checkpoint(
            "cp_start".to_string(),
            Some("Start".to_string()),
            serde_json::json!({"phase": "start"}),
        )
        .unwrap();

    // Events between checkpoints
    recorder.record_event(make_ipc_simple("2", "between_1"));
    recorder.record_event(make_ipc_simple("3", "between_2"));
    recorder.record_event(make_ipc_simple("4", "between_3"));

    recorder
        .checkpoint(
            "cp_end".to_string(),
            Some("End".to_string()),
            serde_json::json!({"phase": "end"}),
        )
        .unwrap();

    // Events after second checkpoint
    recorder.record_event(make_ipc_simple("5", "after_all"));

    let between = recorder
        .events_between_checkpoints("cp_start", "cp_end")
        .unwrap();
    assert_eq!(between.len(), 3);

    // Verify it's the correct events (indices 2, 3, 4)
    assert_eq!(between[0].index, 2);
    assert_eq!(between[1].index, 3);
    assert_eq!(between[2].index, 4);

    // Verify event content
    match &between[0].event {
        AppEvent::Ipc(call) => assert_eq!(call.command, "between_1"),
        _ => panic!("expected IPC"),
    }
    match &between[2].event {
        AppEvent::Ipc(call) => assert_eq!(call.command, "between_3"),
        _ => panic!("expected IPC"),
    }
}

#[test]
fn recorder_events_between_checkpoints_reversed_order() {
    let recorder = EventRecorder::new(1000);
    recorder.start("s1".to_string()).unwrap();

    recorder
        .checkpoint("cp_a".to_string(), None, serde_json::json!(null))
        .unwrap();
    recorder.record_event(make_ipc_simple("1", "mid"));
    recorder
        .checkpoint("cp_b".to_string(), None, serde_json::json!(null))
        .unwrap();

    // Reversed order: from cp_b to cp_a should still work (handled by min/max in source)
    let between = recorder.events_between_checkpoints("cp_b", "cp_a").unwrap();
    assert_eq!(between.len(), 1);
    match &between[0].event {
        AppEvent::Ipc(call) => assert_eq!(call.command, "mid"),
        _ => panic!("expected IPC"),
    }
}

#[test]
fn recorder_events_between_checkpoints_same_checkpoint() {
    let recorder = EventRecorder::new(1000);
    recorder.start("s1".to_string()).unwrap();

    recorder.record_event(make_ipc_simple("1", "before"));
    recorder
        .checkpoint("cp_same".to_string(), None, serde_json::json!(null))
        .unwrap();
    recorder.record_event(make_ipc_simple("2", "after"));

    // Same checkpoint for both from and to => range is [idx, idx) which is empty
    let between = recorder
        .events_between_checkpoints("cp_same", "cp_same")
        .unwrap();
    assert!(between.is_empty());
}

#[test]
fn recorder_events_between_checkpoints_nonexistent_from() {
    let recorder = EventRecorder::new(1000);
    recorder.start("s1".to_string()).unwrap();

    recorder
        .checkpoint("cp_real".to_string(), None, serde_json::json!(null))
        .unwrap();

    let err = recorder
        .events_between_checkpoints("nonexistent", "cp_real")
        .unwrap_err();
    assert!(
        err.to_string().contains("nonexistent"),
        "error should name the missing checkpoint: {err}"
    );
}

#[test]
fn recorder_events_between_checkpoints_nonexistent_to() {
    let recorder = EventRecorder::new(1000);
    recorder.start("s1".to_string()).unwrap();

    recorder
        .checkpoint("cp_real".to_string(), None, serde_json::json!(null))
        .unwrap();

    let err = recorder
        .events_between_checkpoints("cp_real", "nonexistent")
        .unwrap_err();
    assert!(
        err.to_string().contains("nonexistent"),
        "error should name the missing checkpoint: {err}"
    );
}

#[test]
fn recorder_events_between_checkpoints_both_nonexistent() {
    let recorder = EventRecorder::new(1000);
    recorder.start("s1".to_string()).unwrap();

    let err = recorder
        .events_between_checkpoints("fake_a", "fake_b")
        .unwrap_err();
    assert!(
        err.to_string().contains("fake_a"),
        "error should name the first missing checkpoint: {err}"
    );
}

#[test]
fn recorder_events_between_checkpoints_not_recording() {
    let recorder = EventRecorder::new(1000);
    let err = recorder
        .events_between_checkpoints("cp1", "cp2")
        .unwrap_err();
    assert!(
        err.to_string().contains("no active recording"),
        "error should indicate no active recording: {err}"
    );
}

#[test]
fn recorder_events_between_checkpoints_no_events_between() {
    let recorder = EventRecorder::new(1000);
    recorder.start("s1".to_string()).unwrap();

    // Two consecutive checkpoints with no events between
    recorder
        .checkpoint("cp_a".to_string(), None, serde_json::json!(null))
        .unwrap();
    recorder
        .checkpoint("cp_b".to_string(), None, serde_json::json!(null))
        .unwrap();

    let between = recorder.events_between_checkpoints("cp_a", "cp_b").unwrap();
    assert!(between.is_empty());
}

#[test]
fn recorder_event_count_increments() {
    let recorder = EventRecorder::new(1000);

    // Not recording => 0
    assert_eq!(recorder.event_count(), 0);

    recorder.start("s1".to_string()).unwrap();
    assert_eq!(recorder.event_count(), 0);

    recorder.record_event(make_ipc_simple("1", "cmd1"));
    assert_eq!(recorder.event_count(), 1);

    recorder.record_event(make_ipc_simple("2", "cmd2"));
    assert_eq!(recorder.event_count(), 2);

    recorder.record_event(AppEvent::StateChange {
        key: "k".to_string(),
        timestamp: Utc::now(),
        caused_by: None,
    });
    assert_eq!(recorder.event_count(), 3);
}

#[test]
fn recorder_event_count_with_eviction() {
    let recorder = EventRecorder::new(3);
    recorder.start("s1".to_string()).unwrap();

    for i in 0..10 {
        recorder.record_event(make_ipc_simple(&i.to_string(), "cmd"));
    }

    // event_count returns events.len(), which is capped at max_events
    assert_eq!(recorder.event_count(), 3);
}

#[test]
fn recorder_is_recording_lifecycle() {
    let recorder = EventRecorder::new(1000);

    // Initially not recording
    assert!(!recorder.is_recording());

    // Start recording
    recorder.start("s1".to_string()).unwrap();
    assert!(recorder.is_recording());

    // Record some events — still recording
    recorder.record_event(make_ipc_simple("1", "cmd"));
    assert!(recorder.is_recording());

    // Stop recording
    let _ = recorder.stop();
    assert!(!recorder.is_recording());

    // Can restart
    recorder.start("s2".to_string()).unwrap();
    assert!(recorder.is_recording());

    let _ = recorder.stop();
    assert!(!recorder.is_recording());
}

// ── Additional DomSnapshot tests ──────────────────────────────────────────

#[test]
fn dom_snapshot_accessible_text_nested_indentation() {
    let snapshot = DomSnapshot {
        webview_label: "main".to_string(),
        elements: vec![DomElement {
            ref_id: "e0".to_string(),
            tag: "div".to_string(),
            role: Some("navigation".to_string()),
            name: Some("Nav".to_string()),
            text: None,
            value: None,
            enabled: true,
            visible: true,
            focusable: false,
            bounds: None,
            children: vec![DomElement {
                ref_id: "e1".to_string(),
                tag: "ul".to_string(),
                role: Some("list".to_string()),
                name: None,
                text: None,
                value: None,
                enabled: true,
                visible: true,
                focusable: false,
                bounds: None,
                children: vec![DomElement {
                    ref_id: "e2".to_string(),
                    tag: "button".to_string(),
                    role: Some("button".to_string()),
                    name: Some("Home".to_string()),
                    text: Some("Home".to_string()),
                    value: None,
                    enabled: true,
                    visible: true,
                    focusable: true,
                    bounds: None,
                    children: vec![],
                    attributes: BTreeMap::new(),
                }],
                attributes: BTreeMap::new(),
            }],
            attributes: BTreeMap::new(),
        }],
        ref_map: BTreeMap::new(),
    };

    let text = snapshot.to_accessible_text(0);

    // Root at indent 0
    assert!(text.contains("- navigation \"Nav\""));
    // Child at indent 1 (2 spaces)
    assert!(text.contains("  - list"));
    // Grandchild at indent 2 (4 spaces), button gets ref
    assert!(text.contains("    - button \"Home\" [ref=e2]"));
}

#[test]
fn dom_snapshot_accessible_text_skips_invisible_children() {
    let snapshot = DomSnapshot {
        webview_label: "main".to_string(),
        elements: vec![DomElement {
            ref_id: "e0".to_string(),
            tag: "div".to_string(),
            role: None,
            name: None,
            text: None,
            value: None,
            enabled: true,
            visible: true,
            focusable: false,
            bounds: None,
            children: vec![
                DomElement {
                    ref_id: "e1".to_string(),
                    tag: "button".to_string(),
                    role: Some("button".to_string()),
                    name: Some("Visible".to_string()),
                    text: None,
                    value: None,
                    enabled: true,
                    visible: true,
                    focusable: true,
                    bounds: None,
                    children: vec![],
                    attributes: BTreeMap::new(),
                },
                DomElement {
                    ref_id: "e2".to_string(),
                    tag: "button".to_string(),
                    role: Some("button".to_string()),
                    name: Some("Hidden".to_string()),
                    text: None,
                    value: None,
                    enabled: true,
                    visible: false,
                    focusable: true,
                    bounds: None,
                    children: vec![],
                    attributes: BTreeMap::new(),
                },
            ],
            attributes: BTreeMap::new(),
        }],
        ref_map: BTreeMap::new(),
    };

    let text = snapshot.to_accessible_text(0);
    assert!(text.contains("Visible"));
    assert!(!text.contains("Hidden"));
}

#[test]
fn dom_snapshot_accessible_text_ref_on_focusable_and_input() {
    let snapshot = DomSnapshot {
        webview_label: "main".to_string(),
        elements: vec![
            DomElement {
                ref_id: "e0".to_string(),
                tag: "div".to_string(),
                role: None,
                name: Some("Container".to_string()),
                text: None,
                value: None,
                enabled: true,
                visible: true,
                focusable: false, // not focusable, not button/input => no ref
                bounds: None,
                children: vec![],
                attributes: BTreeMap::new(),
            },
            DomElement {
                ref_id: "e1".to_string(),
                tag: "input".to_string(),
                role: Some("textbox".to_string()),
                name: Some("Name".to_string()),
                text: None,
                value: None,
                enabled: true,
                visible: true,
                focusable: false, // input tag => ref regardless of focusable
                bounds: None,
                children: vec![],
                attributes: BTreeMap::new(),
            },
            DomElement {
                ref_id: "e2".to_string(),
                tag: "span".to_string(),
                role: None,
                name: Some("Label".to_string()),
                text: None,
                value: None,
                enabled: true,
                visible: true,
                focusable: true, // focusable => ref
                bounds: None,
                children: vec![],
                attributes: BTreeMap::new(),
            },
        ],
        ref_map: BTreeMap::new(),
    };

    let text = snapshot.to_accessible_text(0);

    // div: not focusable, not button/input => no ref
    assert!(text.contains("div \"Container\""));
    assert!(!text.contains("[ref=e0]"));

    // input: gets ref because tag is "input"
    assert!(text.contains("[ref=e1]"));

    // focusable span: gets ref
    assert!(text.contains("[ref=e2]"));
}

#[test]
fn dom_snapshot_accessible_text_custom_starting_indent() {
    let snapshot = DomSnapshot {
        webview_label: "main".to_string(),
        elements: vec![DomElement {
            ref_id: "e0".to_string(),
            tag: "div".to_string(),
            role: Some("main".to_string()),
            name: None,
            text: None,
            value: None,
            enabled: true,
            visible: true,
            focusable: false,
            bounds: None,
            children: vec![],
            attributes: BTreeMap::new(),
        }],
        ref_map: BTreeMap::new(),
    };

    // Start at indent 3 => 6 spaces prefix
    let text = snapshot.to_accessible_text(3);
    assert!(text.starts_with("      - main\n"));
}

#[test]
fn dom_snapshot_accessible_text_empty_elements() {
    let snapshot = DomSnapshot {
        webview_label: "main".to_string(),
        elements: vec![],
        ref_map: BTreeMap::new(),
    };

    let text = snapshot.to_accessible_text(0);
    assert!(text.is_empty());
}

#[test]
fn dom_snapshot_accessible_text_uses_tag_when_no_role() {
    let snapshot = DomSnapshot {
        webview_label: "main".to_string(),
        elements: vec![DomElement {
            ref_id: "e0".to_string(),
            tag: "section".to_string(),
            role: None, // no role => falls back to tag name
            name: Some("Content".to_string()),
            text: None,
            value: None,
            enabled: true,
            visible: true,
            focusable: false,
            bounds: None,
            children: vec![],
            attributes: BTreeMap::new(),
        }],
        ref_map: BTreeMap::new(),
    };

    let text = snapshot.to_accessible_text(0);
    assert!(text.contains("- section \"Content\""));
}
