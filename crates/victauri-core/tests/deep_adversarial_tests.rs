use std::collections::BTreeMap;
use std::sync::Arc;
use std::thread;

use chrono::Utc;
use victauri_core::event::IpcResult;
use victauri_core::types::DivergenceSeverity;
use victauri_core::verification::AssertionCondition;
use victauri_core::*;

fn ipc(id: &str, cmd: &str) -> AppEvent {
    AppEvent::Ipc(IpcCall {
        id: id.to_string(),
        command: cmd.to_string(),
        timestamp: Utc::now(),
        duration_ms: Some(1),
        result: IpcResult::Ok(serde_json::json!("ok")),
        arg_size_bytes: 0,
        webview_label: "main".to_string(),
    })
}

fn ipc_pending(id: &str, cmd: &str) -> AppEvent {
    AppEvent::Ipc(IpcCall {
        id: id.to_string(),
        command: cmd.to_string(),
        timestamp: Utc::now(),
        duration_ms: None,
        result: IpcResult::Pending,
        arg_size_bytes: 0,
        webview_label: "main".to_string(),
    })
}

fn ipc_err(id: &str, cmd: &str, err: &str) -> AppEvent {
    AppEvent::Ipc(IpcCall {
        id: id.to_string(),
        command: cmd.to_string(),
        timestamp: Utc::now(),
        duration_ms: Some(1),
        result: IpcResult::Err(err.to_string()),
        arg_size_bytes: 0,
        webview_label: "main".to_string(),
    })
}

fn state_change(key: &str) -> AppEvent {
    AppEvent::StateChange {
        key: key.to_string(),
        timestamp: Utc::now(),
        caused_by: None,
    }
}

fn dom_mutation(count: u32) -> AppEvent {
    AppEvent::DomMutation {
        webview_label: "main".to_string(),
        timestamp: Utc::now(),
        mutation_count: count,
    }
}

fn window_event(label: &str, event: &str) -> AppEvent {
    AppEvent::WindowEvent {
        label: label.to_string(),
        event: event.to_string(),
        timestamp: Utc::now(),
    }
}

fn dom_interaction(action: InteractionKind, selector: &str) -> AppEvent {
    AppEvent::DomInteraction {
        action,
        selector: selector.to_string(),
        value: None,
        timestamp: Utc::now(),
        webview_label: "main".to_string(),
    }
}

fn element(ref_id: &str, tag: &str) -> DomElement {
    DomElement {
        ref_id: ref_id.to_string(),
        tag: tag.to_string(),
        role: None,
        name: None,
        text: None,
        value: None,
        enabled: true,
        visible: true,
        focusable: false,
        bounds: None,
        children: vec![],
        attributes: BTreeMap::new(),
    }
}

fn assertion(
    label: &str,
    cond: AssertionCondition,
    expected: serde_json::Value,
) -> SemanticAssertion {
    SemanticAssertion {
        label: label.to_string(),
        condition: cond,
        expected,
    }
}

// ── Group 1: EventLog Ring Buffer Adversarial ────────────────────────────

#[test]
fn eventlog_push_exactly_at_capacity_then_one_more() {
    let log = EventLog::new(5);
    for i in 0..5 {
        log.push(ipc(&i.to_string(), &format!("cmd_{i}")));
    }
    assert_eq!(log.len(), 5);
    log.push(ipc("5", "cmd_5"));
    assert_eq!(log.len(), 5);
    let calls = log.ipc_calls();
    assert_eq!(calls[0].command, "cmd_1");
    assert_eq!(calls[4].command, "cmd_5");
}

#[test]
fn eventlog_push_millions_stays_at_capacity() {
    let log = EventLog::new(100);
    for i in 0..100_000 {
        log.push(ipc(&i.to_string(), "spam"));
    }
    assert_eq!(log.len(), 100);
    let calls = log.ipc_calls();
    assert_eq!(calls[0].id, "99900");
    assert_eq!(calls[99].id, "99999");
}

#[test]
fn eventlog_drain_to_vec_preserves_ordering() {
    let log = EventLog::new(10);
    for i in 0..15 {
        log.push(ipc(&i.to_string(), &format!("cmd_{i}")));
    }
    let snap = log.snapshot();
    assert_eq!(snap.len(), 10);
    for pair in snap.windows(2) {
        let id_a: usize = match &pair[0] {
            AppEvent::Ipc(c) => c.id.parse().unwrap(),
            _ => panic!(),
        };
        let id_b: usize = match &pair[1] {
            AppEvent::Ipc(c) => c.id.parse().unwrap(),
            _ => panic!(),
        };
        assert!(id_b > id_a);
    }
}

#[test]
fn eventlog_clear_empties_completely() {
    let log = EventLog::new(100);
    for i in 0..50 {
        log.push(ipc(&i.to_string(), "cmd"));
    }
    log.clear();
    assert!(log.is_empty());
    assert_eq!(log.len(), 0);
    assert!(log.snapshot().is_empty());
    assert!(log.ipc_calls().is_empty());
    assert!(
        log.since(Utc::now() - chrono::Duration::hours(1))
            .is_empty()
    );
}

#[test]
fn eventlog_push_all_event_variants() {
    let log = EventLog::new(100);
    log.push(ipc("1", "cmd"));
    log.push(state_change("key1"));
    log.push(dom_mutation(5));
    log.push(window_event("main", "focus"));
    log.push(dom_interaction(InteractionKind::Click, "#btn"));
    assert_eq!(log.len(), 5);
    let snap = log.snapshot();
    assert!(matches!(&snap[0], AppEvent::Ipc(_)));
    assert!(matches!(&snap[1], AppEvent::StateChange { .. }));
    assert!(matches!(&snap[2], AppEvent::DomMutation { .. }));
    assert!(matches!(&snap[3], AppEvent::WindowEvent { .. }));
    assert!(matches!(&snap[4], AppEvent::DomInteraction { .. }));
}

#[test]
fn eventlog_empty_log_operations() {
    let log = EventLog::new(100);
    assert!(log.is_empty());
    assert_eq!(log.len(), 0);
    assert!(log.snapshot().is_empty());
    assert!(log.ipc_calls().is_empty());
    assert!(log.snapshot_range(0, 10).is_empty());
    assert!(log.since(Utc::now()).is_empty());
    assert!(log.ipc_calls_since(Utc::now()).is_empty());
}

#[test]
fn eventlog_capacity_one() {
    let log = EventLog::new(1);
    assert_eq!(log.capacity(), 1);
    log.push(ipc("1", "first"));
    assert_eq!(log.len(), 1);
    log.push(ipc("2", "second"));
    assert_eq!(log.len(), 1);
    log.push(ipc("3", "third"));
    assert_eq!(log.len(), 1);
    let calls = log.ipc_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].command, "third");
}

#[test]
fn eventlog_filter_ipc_after_wraparound() {
    let log = EventLog::new(5);
    log.push(ipc("0", "ipc_0"));
    log.push(state_change("s1"));
    log.push(ipc("1", "ipc_1"));
    log.push(state_change("s2"));
    log.push(ipc("2", "ipc_2"));
    log.push(state_change("s3"));
    log.push(ipc("3", "ipc_3"));
    assert_eq!(log.len(), 5);
    let calls = log.ipc_calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].command, "ipc_1");
    assert_eq!(calls[1].command, "ipc_2");
    assert_eq!(calls[2].command, "ipc_3");
}

#[test]
fn eventlog_snapshot_range_after_wraparound() {
    let log = EventLog::new(5);
    for i in 0..10 {
        log.push(ipc(&i.to_string(), &format!("cmd_{i}")));
    }
    let page = log.snapshot_range(2, 2);
    assert_eq!(page.len(), 2);
    match &page[0] {
        AppEvent::Ipc(c) => assert_eq!(c.command, "cmd_7"),
        _ => panic!(),
    }
}

#[test]
fn eventlog_since_returns_events_at_exact_timestamp() {
    let ts = Utc::now();
    let log = EventLog::new(100);
    log.push(AppEvent::StateChange {
        key: "exact".to_string(),
        timestamp: ts,
        caused_by: None,
    });
    let result = log.since(ts);
    assert_eq!(result.len(), 1);
}

#[test]
fn eventlog_ipc_calls_since_ignores_non_ipc() {
    let log = EventLog::new(100);
    let ts = Utc::now();
    log.push(state_change("k"));
    log.push(dom_mutation(1));
    log.push(window_event("main", "focus"));
    let calls = log.ipc_calls_since(ts - chrono::Duration::seconds(1));
    assert!(calls.is_empty());
}

#[test]
fn eventlog_clear_then_push_reuses_capacity() {
    let log = EventLog::new(3);
    for i in 0..3 {
        log.push(ipc(&i.to_string(), "old"));
    }
    log.clear();
    for i in 0..5 {
        log.push(ipc(&i.to_string(), "new"));
    }
    assert_eq!(log.len(), 3);
    let calls = log.ipc_calls();
    assert_eq!(calls[0].id, "2");
}

#[test]
fn eventlog_snapshot_range_zero_limit() {
    let log = EventLog::new(100);
    log.push(ipc("1", "cmd"));
    assert!(log.snapshot_range(0, 0).is_empty());
}

#[test]
fn eventlog_mixed_eviction_preserves_non_ipc() {
    let log = EventLog::new(3);
    log.push(state_change("s1"));
    log.push(dom_mutation(1));
    log.push(window_event("main", "blur"));
    log.push(ipc("1", "late_ipc"));
    assert_eq!(log.len(), 3);
    let snap = log.snapshot();
    assert!(matches!(&snap[0], AppEvent::DomMutation { .. }));
    assert!(matches!(&snap[1], AppEvent::WindowEvent { .. }));
    assert!(matches!(&snap[2], AppEvent::Ipc(_)));
}

#[test]
fn eventlog_dom_interaction_variants_survive_eviction() {
    let log = EventLog::new(2);
    log.push(dom_interaction(InteractionKind::Click, "#a"));
    log.push(dom_interaction(InteractionKind::Fill, "#b"));
    log.push(dom_interaction(InteractionKind::KeyPress, "#c"));
    assert_eq!(log.len(), 2);
    let snap = log.snapshot();
    match &snap[0] {
        AppEvent::DomInteraction { action, .. } => assert_eq!(*action, InteractionKind::Fill),
        _ => panic!(),
    }
}

// ── Group 2: CommandRegistry Adversarial ──────────────────────────────────

#[test]
fn registry_register_same_name_twice_overwrites() {
    let reg = CommandRegistry::new();
    reg.register(CommandInfo::new("cmd").with_description("first"));
    reg.register(CommandInfo::new("cmd").with_description("second"));
    assert_eq!(reg.count(), 1);
    let cmd = reg.get("cmd").unwrap();
    assert_eq!(cmd.description.as_deref(), Some("second"));
}

#[test]
fn registry_empty_name() {
    let reg = CommandRegistry::new();
    reg.register(CommandInfo::new(""));
    assert_eq!(reg.count(), 1);
    assert!(reg.get("").is_some());
}

#[test]
fn registry_empty_description() {
    let reg = CommandRegistry::new();
    reg.register(CommandInfo::new("cmd").with_description(""));
    let cmd = reg.get("cmd").unwrap();
    assert_eq!(cmd.description.as_deref(), Some(""));
}

#[test]
fn registry_very_long_name() {
    let reg = CommandRegistry::new();
    let long_name = "a".repeat(10_000);
    reg.register(CommandInfo::new(&long_name));
    assert_eq!(reg.count(), 1);
    assert!(reg.get(&long_name).is_some());
}

#[test]
fn registry_search_empty_query() {
    let reg = CommandRegistry::new();
    reg.register(CommandInfo::new("cmd").with_description("test"));
    let results = reg.search("");
    assert_eq!(results.len(), 1);
}

#[test]
fn registry_search_special_regex_chars() {
    let reg = CommandRegistry::new();
    reg.register(CommandInfo::new("cmd[0]").with_description("brackets"));
    let results = reg.search("[0]");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "cmd[0]");
}

#[test]
fn registry_resolve_vague_query() {
    let reg = CommandRegistry::new();
    reg.register(CommandInfo::new("save_file").with_description("Save a file to disk"));
    reg.register(CommandInfo::new("load_file").with_description("Load a file from disk"));
    reg.register(CommandInfo::new("delete_file").with_description("Delete a file from disk"));
    let results = reg.resolve("file");
    assert_eq!(results.len(), 3);
}

#[test]
fn registry_resolve_returns_sorted_by_score() {
    let reg = CommandRegistry::new();
    reg.register(
        CommandInfo::new("save_file")
            .with_description("Save file")
            .with_intent("persist file"),
    );
    reg.register(CommandInfo::new("unrelated").with_description("Something else entirely"));
    let results = reg.resolve("save file");
    assert!(!results.is_empty());
    assert_eq!(results[0].command.name, "save_file");
    for pair in results.windows(2) {
        assert!(pair[0].score >= pair[1].score);
    }
}

#[test]
fn registry_register_10000_commands_search() {
    let reg = CommandRegistry::new();
    for i in 0..10_000 {
        reg.register(
            CommandInfo::new(format!("cmd_{i}")).with_description(format!("Command number {i}")),
        );
    }
    assert_eq!(reg.count(), 10_000);
    let results = reg.search("cmd_9999");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "cmd_9999");
}

#[test]
fn registry_command_info_all_optional_fields() {
    let reg = CommandRegistry::new();
    let mut cmd = CommandInfo::new("full_cmd")
        .with_description("Full description")
        .with_intent("do everything")
        .with_category("admin");
    cmd.plugin = Some("my_plugin".to_string());
    cmd.args = vec![
        CommandArg {
            name: "arg1".to_string(),
            type_name: "String".to_string(),
            required: true,
            schema: Some(serde_json::json!({"type": "string"})),
        },
        CommandArg {
            name: "arg2".to_string(),
            type_name: "Option<u32>".to_string(),
            required: false,
            schema: None,
        },
    ];
    cmd.return_type = Some("Result<(), Error>".to_string());
    cmd.is_async = true;
    cmd.examples = vec!["do the thing".to_string(), "run full_cmd".to_string()];
    reg.register(cmd);
    let fetched = reg.get("full_cmd").unwrap();
    assert_eq!(fetched.plugin.as_deref(), Some("my_plugin"));
    assert_eq!(fetched.args.len(), 2);
    assert!(fetched.is_async);
    assert_eq!(fetched.examples.len(), 2);
    assert_eq!(fetched.category.as_deref(), Some("admin"));
}

#[test]
fn registry_unicode_in_names_and_descriptions() {
    let reg = CommandRegistry::new();
    reg.register(
        CommandInfo::new("sauvegarde_donnees")
            .with_description("Sauvegarder les donnees utilisateur")
            .with_intent("enregistrer les preferences")
            .with_category("parametres"),
    );
    assert!(reg.get("sauvegarde_donnees").is_some());
    let results = reg.search("donnees");
    assert_eq!(results.len(), 1);
}

#[test]
fn registry_unicode_emoji_in_description() {
    let reg = CommandRegistry::new();
    reg.register(CommandInfo::new("rocket").with_description("Launch the rocket \u{1F680}"));
    let results = reg.search("\u{1F680}");
    assert_eq!(results.len(), 1);
}

#[test]
fn registry_resolve_example_match() {
    let reg = CommandRegistry::new();
    let mut cmd = CommandInfo::new("export_csv");
    cmd.examples = vec!["download data as csv".to_string()];
    reg.register(cmd);
    let results = reg.resolve("download data as csv");
    assert!(!results.is_empty());
    assert_eq!(results[0].command.name, "export_csv");
}

#[test]
fn registry_list_returns_alphabetical() {
    let reg = CommandRegistry::new();
    reg.register(CommandInfo::new("zebra"));
    reg.register(CommandInfo::new("alpha"));
    reg.register(CommandInfo::new("mango"));
    let list = reg.list();
    assert_eq!(list[0].name, "alpha");
    assert_eq!(list[1].name, "mango");
    assert_eq!(list[2].name, "zebra");
}

#[test]
fn registry_get_nonexistent_returns_none() {
    let reg = CommandRegistry::new();
    assert!(reg.get("does_not_exist").is_none());
}

#[test]
fn registry_search_case_insensitive() {
    let reg = CommandRegistry::new();
    reg.register(CommandInfo::new("GetUserProfile").with_description("Retrieves USER profile"));
    let results = reg.search("GETUSERPROFILE");
    assert_eq!(results.len(), 1);
    let results2 = reg.search("user");
    assert_eq!(results2.len(), 1);
}

// ── Group 3: EventRecorder Adversarial ───────────────────────────────────

#[test]
fn recorder_start_stop_lifecycle() {
    let rec = EventRecorder::new(100);
    assert!(!rec.is_recording());
    rec.start("s1".to_string()).unwrap();
    assert!(rec.is_recording());
    let session = rec.stop().unwrap();
    assert_eq!(session.id, "s1");
    assert!(!rec.is_recording());
}

#[test]
fn recorder_start_twice_returns_error() {
    let rec = EventRecorder::new(100);
    rec.start("s1".to_string()).unwrap();
    let err = rec.start("s2".to_string()).unwrap_err();
    assert!(err.to_string().contains("already active"));
}

#[test]
fn recorder_stop_twice_returns_none() {
    let rec = EventRecorder::new(100);
    rec.start("s1".to_string()).unwrap();
    assert!(rec.stop().is_some());
    assert!(rec.stop().is_none());
}

#[test]
fn recorder_checkpoint_without_recording() {
    let rec = EventRecorder::new(100);
    let err = rec
        .checkpoint("cp".to_string(), None, serde_json::json!({}))
        .unwrap_err();
    assert!(err.to_string().contains("no active recording"));
}

#[test]
fn recorder_record_50000_events_at_capacity() {
    let rec = EventRecorder::new(50_000);
    rec.start("big".to_string()).unwrap();
    for i in 0..60_000 {
        rec.record_event(ipc(&i.to_string(), "spam"));
    }
    assert_eq!(rec.event_count(), 50_000);
    let session = rec.stop().unwrap();
    assert_eq!(session.events.len(), 50_000);
    assert_eq!(session.events[0].index, 10_000);
    assert_eq!(session.events[49_999].index, 59_999);
}

#[test]
fn recorder_export_json_roundtrip() {
    let rec = EventRecorder::new(100);
    rec.start("serde".to_string()).unwrap();
    rec.record_event(ipc("1", "cmd_a"));
    rec.record_event(state_change("key"));
    rec.checkpoint(
        "cp1".to_string(),
        Some("label".to_string()),
        serde_json::json!({"x": 1}),
    )
    .unwrap();
    let exported = rec.export().unwrap();
    let json = serde_json::to_string(&exported).unwrap();
    let roundtripped: RecordedSession = serde_json::from_str(&json).unwrap();
    assert_eq!(roundtripped.id, "serde");
    assert_eq!(roundtripped.events.len(), 2);
    assert_eq!(roundtripped.checkpoints.len(), 1);
    assert_eq!(roundtripped.checkpoints[0].id, "cp1");
    assert_eq!(
        roundtripped.checkpoints[0].state,
        serde_json::json!({"x": 1})
    );
}

#[test]
fn recorder_import_invalid_session_empty() {
    let rec = EventRecorder::new(100);
    let session = RecordedSession {
        id: "empty".to_string(),
        started_at: Utc::now(),
        events: vec![],
        checkpoints: vec![],
    };
    rec.import(session);
    assert!(rec.is_recording());
    assert_eq!(rec.event_count(), 0);
    rec.record_event(ipc("1", "after_import"));
    assert_eq!(rec.event_count(), 1);
}

#[test]
fn recorder_events_between_same_checkpoint() {
    let rec = EventRecorder::new(100);
    rec.start("s".to_string()).unwrap();
    rec.record_event(ipc("1", "before"));
    rec.checkpoint("cp".to_string(), None, serde_json::json!(null))
        .unwrap();
    rec.record_event(ipc("2", "after"));
    let between = rec.events_between_checkpoints("cp", "cp").unwrap();
    assert!(between.is_empty());
}

#[test]
fn recorder_events_between_non_adjacent_checkpoints() {
    let rec = EventRecorder::new(100);
    rec.start("s".to_string()).unwrap();
    rec.checkpoint("cp1".to_string(), None, serde_json::json!(null))
        .unwrap();
    rec.record_event(ipc("1", "a"));
    rec.checkpoint("cp2".to_string(), None, serde_json::json!(null))
        .unwrap();
    rec.record_event(ipc("2", "b"));
    rec.checkpoint("cp3".to_string(), None, serde_json::json!(null))
        .unwrap();
    let between = rec.events_between_checkpoints("cp1", "cp3").unwrap();
    assert_eq!(between.len(), 2);
}

#[test]
fn recorder_checkpoint_ordering_with_rapid_insertion() {
    let rec = EventRecorder::new(10_000);
    rec.start("rapid".to_string()).unwrap();
    for i in 0..100 {
        rec.record_event(ipc(&i.to_string(), &format!("cmd_{i}")));
        rec.checkpoint(format!("cp_{i}"), None, serde_json::json!(i))
            .unwrap();
    }
    let cps = rec.get_checkpoints();
    assert_eq!(cps.len(), 100);
    for pair in cps.windows(2) {
        assert!(pair[1].event_index > pair[0].event_index);
    }
}

#[test]
fn recorder_empty_checkpoint_list() {
    let rec = EventRecorder::new(100);
    rec.start("s".to_string()).unwrap();
    rec.record_event(ipc("1", "cmd"));
    assert_eq!(rec.checkpoint_count(), 0);
    assert!(rec.get_checkpoints().is_empty());
}

#[test]
fn recorder_events_since_with_eviction() {
    let rec = EventRecorder::new(5);
    rec.start("s".to_string()).unwrap();
    for i in 0..10 {
        rec.record_event(ipc(&i.to_string(), &format!("cmd_{i}")));
    }
    let since_0 = rec.events_since(0);
    assert_eq!(since_0.len(), 5);
    assert_eq!(since_0[0].index, 5);
    let since_8 = rec.events_since(8);
    assert_eq!(since_8.len(), 2);
}

#[test]
fn recorder_ipc_replay_filters_non_ipc() {
    let rec = EventRecorder::new(100);
    rec.start("s".to_string()).unwrap();
    rec.record_event(ipc("1", "cmd_a"));
    rec.record_event(state_change("k"));
    rec.record_event(dom_mutation(3));
    rec.record_event(ipc("2", "cmd_b"));
    rec.record_event(window_event("main", "resize"));
    let replay = rec.ipc_replay_sequence();
    assert_eq!(replay.len(), 2);
    assert_eq!(replay[0].command, "cmd_a");
    assert_eq!(replay[1].command, "cmd_b");
}

#[test]
fn recorder_not_recording_all_queries_return_empty() {
    let rec = EventRecorder::new(100);
    assert_eq!(rec.event_count(), 0);
    assert_eq!(rec.checkpoint_count(), 0);
    assert!(rec.events_since(0).is_empty());
    assert!(rec.ipc_replay_sequence().is_empty());
    assert!(rec.get_checkpoints().is_empty());
    assert!(rec.export().is_none());
    assert!(rec.stop().is_none());
    assert!(rec.events_between(Utc::now(), Utc::now()).is_empty());
}

#[test]
fn recorder_restart_after_stop() {
    let rec = EventRecorder::new(100);
    rec.start("s1".to_string()).unwrap();
    rec.record_event(ipc("1", "old"));
    let _ = rec.stop();
    rec.start("s2".to_string()).unwrap();
    assert_eq!(rec.event_count(), 0);
    rec.record_event(ipc("2", "new"));
    assert_eq!(rec.event_count(), 1);
    let session = rec.stop().unwrap();
    assert_eq!(session.id, "s2");
    assert_eq!(session.events.len(), 1);
}

#[test]
fn recorder_export_does_not_consume_session() {
    let rec = EventRecorder::new(100);
    rec.start("s".to_string()).unwrap();
    rec.record_event(ipc("1", "cmd"));
    let _e1 = rec.export().unwrap();
    let _e2 = rec.export().unwrap();
    assert!(rec.is_recording());
    assert_eq!(rec.event_count(), 1);
}

#[test]
fn recorder_import_replaces_active() {
    let rec = EventRecorder::new(100);
    rec.start("original".to_string()).unwrap();
    rec.record_event(ipc("1", "orig_cmd"));
    let session = RecordedSession {
        id: "imported".to_string(),
        started_at: Utc::now(),
        events: vec![
            RecordedEvent {
                index: 0,
                timestamp: Utc::now(),
                event: ipc("10", "imp_a"),
            },
            RecordedEvent {
                index: 1,
                timestamp: Utc::now(),
                event: ipc("11", "imp_b"),
            },
            RecordedEvent {
                index: 2,
                timestamp: Utc::now(),
                event: ipc("12", "imp_c"),
            },
        ],
        checkpoints: vec![],
    };
    rec.import(session);
    assert_eq!(rec.event_count(), 3);
    let stopped = rec.stop().unwrap();
    assert_eq!(stopped.id, "imported");
}

#[test]
fn recorder_events_between_timestamps() {
    let rec = EventRecorder::new(100);
    rec.start("s".to_string()).unwrap();
    let t1 = Utc::now();
    std::thread::sleep(std::time::Duration::from_millis(50));
    rec.record_event(ipc("1", "cmd_a"));
    std::thread::sleep(std::time::Duration::from_millis(50));
    let t2 = Utc::now();
    std::thread::sleep(std::time::Duration::from_millis(50));
    rec.record_event(ipc("2", "cmd_b"));
    std::thread::sleep(std::time::Duration::from_millis(50));
    let t3 = Utc::now();
    let between = rec.events_between(t1, t3);
    assert_eq!(between.len(), 2);
    let between_narrow = rec.events_between(t2, t3);
    assert_eq!(between_narrow.len(), 1);
}

#[test]
fn recorder_default_has_50000_capacity() {
    let rec = EventRecorder::default();
    rec.start("s".to_string()).unwrap();
    for i in 0..100 {
        rec.record_event(ipc(&i.to_string(), "cmd"));
    }
    assert_eq!(rec.event_count(), 100);
}

// ── Group 4: DomSnapshot / DomElement ────────────────────────────────────

#[test]
fn dom_deep_nesting_is_depth_bounded() {
    // A snapshot can come from a hostile page (browser-extension target); rendering
    // its accessible text must be depth-bounded, not an unbounded recursion that
    // stack-overflows the host (audit #21).
    fn build_nested(depth: usize) -> DomElement {
        let mut el = element(&format!("e{depth}"), "div");
        el.visible = true;
        if depth > 0 {
            el.children = vec![build_nested(depth - 1)];
        }
        el
    }
    // 1000 levels > the 256 DOM cap (so the guard fires) but is safe to drop.
    let snap = DomSnapshot {
        webview_label: "main".to_string(),
        elements: vec![build_nested(1000)],
        ref_map: BTreeMap::new(),
    };
    let text = snap.to_accessible_text(0); // guard prevents recursing past the cap
    assert!(
        text.contains("max DOM depth"),
        "expected the depth guard to fire"
    );
    assert!(
        text.lines().count() < 300,
        "expected bounded output, got {} lines",
        text.lines().count()
    );
}

#[test]
fn dom_wide_tree_1000_children() {
    let children: Vec<DomElement> = (0..1000)
        .map(|i| {
            let mut el = element(&format!("c{i}"), "span");
            el.visible = true;
            el.name = Some(format!("Child {i}"));
            el
        })
        .collect();
    let mut root = element("root", "div");
    root.visible = true;
    root.children = children;
    let snap = DomSnapshot {
        webview_label: "main".to_string(),
        elements: vec![root],
        ref_map: BTreeMap::new(),
    };
    let text = snap.to_accessible_text(0);
    assert!(text.contains("Child 0"));
    assert!(text.contains("Child 999"));
}

#[test]
fn dom_empty_tree() {
    let snap = DomSnapshot {
        webview_label: "main".to_string(),
        elements: vec![],
        ref_map: BTreeMap::new(),
    };
    assert!(snap.to_accessible_text(0).is_empty());
}

#[test]
fn dom_element_all_optional_fields() {
    let el = DomElement {
        ref_id: "e1".to_string(),
        tag: "input".to_string(),
        role: Some("textbox".to_string()),
        name: Some("Email".to_string()),
        text: Some("user@example.com".to_string()),
        value: Some("user@example.com".to_string()),
        enabled: true,
        visible: true,
        focusable: true,
        bounds: Some(snapshot::ElementBounds {
            x: 10.0,
            y: 20.0,
            width: 200.0,
            height: 30.0,
        }),
        children: vec![],
        attributes: {
            let mut m = BTreeMap::new();
            m.insert("type".to_string(), "email".to_string());
            m.insert("placeholder".to_string(), "Enter email".to_string());
            m
        },
    };
    let snap = DomSnapshot {
        webview_label: "main".to_string(),
        elements: vec![el],
        ref_map: BTreeMap::new(),
    };
    let text = snap.to_accessible_text(0);
    assert!(text.contains("textbox"));
    assert!(text.contains("Email"));
    assert!(text.contains("[ref=e1]"));
}

#[test]
fn dom_element_minimal() {
    let el = element("e0", "div");
    let snap = DomSnapshot {
        webview_label: "main".to_string(),
        elements: vec![el],
        ref_map: BTreeMap::new(),
    };
    let text = snap.to_accessible_text(0);
    assert!(text.contains("div"));
}

#[test]
fn dom_ref_handle_uniqueness() {
    let children: Vec<DomElement> = (0..100)
        .map(|i| {
            let mut el = element(&format!("e{i}"), "button");
            el.visible = true;
            el.focusable = true;
            el
        })
        .collect();
    let mut root = element("root", "div");
    root.visible = true;
    root.children = children;
    let snap = DomSnapshot {
        webview_label: "main".to_string(),
        elements: vec![root],
        ref_map: BTreeMap::new(),
    };
    let text = snap.to_accessible_text(0);
    for i in 0..100 {
        assert!(text.contains(&format!("[ref=e{i}]")));
    }
}

#[test]
fn dom_serde_roundtrip() {
    let snap = DomSnapshot {
        webview_label: "main".to_string(),
        elements: vec![DomElement {
            ref_id: "e0".to_string(),
            tag: "div".to_string(),
            role: Some("main".to_string()),
            name: Some("Content".to_string()),
            text: Some("Hello".to_string()),
            value: None,
            enabled: true,
            visible: true,
            focusable: false,
            bounds: Some(snapshot::ElementBounds {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 1080.0,
            }),
            children: vec![{
                let mut child = element("e1", "button");
                child.role = Some("button".to_string());
                child.name = Some("Click me".to_string());
                child.focusable = true;
                child.visible = true;
                child
            }],
            attributes: BTreeMap::new(),
        }],
        ref_map: {
            let mut m = BTreeMap::new();
            m.insert("e0".to_string(), "div.main".to_string());
            m.insert("e1".to_string(), "button.click".to_string());
            m
        },
    };
    let json = serde_json::to_string(&snap).unwrap();
    let roundtripped: DomSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(roundtripped.webview_label, "main");
    assert_eq!(roundtripped.elements.len(), 1);
    assert_eq!(roundtripped.elements[0].children.len(), 1);
    assert_eq!(roundtripped.ref_map.len(), 2);
}

#[test]
fn dom_invisible_parent_hides_subtree() {
    let mut parent = element("e0", "div");
    parent.visible = false;
    let mut child = element("e1", "button");
    child.visible = true;
    child.name = Some("Hidden button".to_string());
    parent.children = vec![child];
    let snap = DomSnapshot {
        webview_label: "main".to_string(),
        elements: vec![parent],
        ref_map: BTreeMap::new(),
    };
    let text = snap.to_accessible_text(0);
    assert!(!text.contains("Hidden button"));
}

#[test]
fn dom_element_bounds_serde() {
    let bounds = snapshot::ElementBounds {
        x: -10.5,
        y: 0.0,
        width: 100.123,
        height: 50.999,
    };
    let json = serde_json::to_string(&bounds).unwrap();
    let rt: snapshot::ElementBounds = serde_json::from_str(&json).unwrap();
    assert_eq!(rt.x, -10.5);
    assert_eq!(rt.width, 100.123);
}

// ── Group 5: Verification & Assertion ────────────────────────────────────

#[test]
fn verify_zero_divergences_passes() {
    let result = verify_state(serde_json::json!({"a": 1}), serde_json::json!({"a": 1}));
    assert!(result.passed);
    assert!(result.divergences.is_empty());
}

#[test]
fn verify_multiple_severity_levels() {
    let frontend = serde_json::json!({"a": 1, "b": null, "c": "hello"});
    let backend = serde_json::json!({"a": 2, "b": 5, "c": 42});
    let result = verify_state(frontend, backend);
    assert!(!result.passed);
    let severities: Vec<&DivergenceSeverity> =
        result.divergences.iter().map(|d| &d.severity).collect();
    assert!(severities.contains(&&DivergenceSeverity::Error));
    assert!(severities.contains(&&DivergenceSeverity::Warning));
}

#[test]
fn verify_deeply_nested_path() {
    let frontend = serde_json::json!({"l1": {"l2": {"l3": {"l4": {"l5": 1}}}}});
    let backend = serde_json::json!({"l1": {"l2": {"l3": {"l4": {"l5": 2}}}}});
    let result = verify_state(frontend, backend);
    assert_eq!(result.divergences[0].path, "l1.l2.l3.l4.l5");
}

#[test]
fn assertion_equals_pass_and_fail() {
    let a = assertion("eq", AssertionCondition::Equals, serde_json::json!(42));
    assert!(evaluate_assertion(serde_json::json!(42), &a).passed);
    assert!(!evaluate_assertion(serde_json::json!(43), &a).passed);
}

#[test]
fn assertion_not_equals() {
    let a = assertion("neq", AssertionCondition::NotEquals, serde_json::json!("a"));
    assert!(evaluate_assertion(serde_json::json!("b"), &a).passed);
    assert!(!evaluate_assertion(serde_json::json!("a"), &a).passed);
}

#[test]
fn assertion_contains_string() {
    let a = assertion(
        "contains",
        AssertionCondition::Contains,
        serde_json::json!("world"),
    );
    assert!(evaluate_assertion(serde_json::json!("hello world"), &a).passed);
    assert!(!evaluate_assertion(serde_json::json!("hello"), &a).passed);
}

#[test]
fn assertion_contains_array() {
    let a = assertion("arr", AssertionCondition::Contains, serde_json::json!(3));
    assert!(evaluate_assertion(serde_json::json!([1, 2, 3]), &a).passed);
    assert!(!evaluate_assertion(serde_json::json!([1, 2, 4]), &a).passed);
}

#[test]
fn assertion_greater_than() {
    let a = assertion("gt", AssertionCondition::GreaterThan, serde_json::json!(10));
    assert!(evaluate_assertion(serde_json::json!(11), &a).passed);
    assert!(!evaluate_assertion(serde_json::json!(10), &a).passed);
    assert!(!evaluate_assertion(serde_json::json!(9), &a).passed);
}

#[test]
fn assertion_less_than() {
    let a = assertion("lt", AssertionCondition::LessThan, serde_json::json!(10));
    assert!(evaluate_assertion(serde_json::json!(9), &a).passed);
    assert!(!evaluate_assertion(serde_json::json!(10), &a).passed);
    assert!(!evaluate_assertion(serde_json::json!(11), &a).passed);
}

#[test]
fn assertion_truthy_all_cases() {
    let a = assertion("t", AssertionCondition::Truthy, serde_json::Value::Null);
    assert!(evaluate_assertion(serde_json::json!(true), &a).passed);
    assert!(evaluate_assertion(serde_json::json!(1), &a).passed);
    assert!(evaluate_assertion(serde_json::json!(-1), &a).passed);
    assert!(evaluate_assertion(serde_json::json!("x"), &a).passed);
    assert!(evaluate_assertion(serde_json::json!([1]), &a).passed);
    assert!(evaluate_assertion(serde_json::json!({"k": "v"}), &a).passed);
    assert!(!evaluate_assertion(serde_json::json!(null), &a).passed);
    assert!(!evaluate_assertion(serde_json::json!(false), &a).passed);
    assert!(!evaluate_assertion(serde_json::json!(0), &a).passed);
    assert!(!evaluate_assertion(serde_json::json!(""), &a).passed);
}

#[test]
fn assertion_falsy_all_cases() {
    let a = assertion("f", AssertionCondition::Falsy, serde_json::Value::Null);
    assert!(evaluate_assertion(serde_json::json!(null), &a).passed);
    assert!(evaluate_assertion(serde_json::json!(false), &a).passed);
    assert!(evaluate_assertion(serde_json::json!(0), &a).passed);
    assert!(evaluate_assertion(serde_json::json!(""), &a).passed);
    assert!(!evaluate_assertion(serde_json::json!(true), &a).passed);
    assert!(!evaluate_assertion(serde_json::json!(1), &a).passed);
    assert!(!evaluate_assertion(serde_json::json!("x"), &a).passed);
}

#[test]
fn assertion_exists() {
    let a = assertion(
        "exists",
        AssertionCondition::Exists,
        serde_json::Value::Null,
    );
    assert!(evaluate_assertion(serde_json::json!("something"), &a).passed);
    assert!(evaluate_assertion(serde_json::json!(0), &a).passed);
    assert!(evaluate_assertion(serde_json::json!(false), &a).passed);
    assert!(!evaluate_assertion(serde_json::Value::Null, &a).passed);
}

#[test]
fn assertion_type_is_all_types() {
    let cases = vec![
        ("string", serde_json::json!("hello"), true),
        ("string", serde_json::json!(42), false),
        ("number", serde_json::json!(42), true),
        ("number", serde_json::json!("42"), false),
        ("boolean", serde_json::json!(true), true),
        ("boolean", serde_json::json!(1), false),
        ("array", serde_json::json!([1, 2]), true),
        ("array", serde_json::json!({}), false),
        ("object", serde_json::json!({"k": "v"}), true),
        ("object", serde_json::json!([]), false),
        ("null", serde_json::Value::Null, true),
        ("null", serde_json::json!(0), false),
    ];
    for (type_name, val, expected) in cases {
        let a = assertion(
            "type",
            AssertionCondition::TypeIs,
            serde_json::json!(type_name),
        );
        let result = evaluate_assertion(val.clone(), &a);
        assert_eq!(result.passed, expected, "TypeIs({type_name}) for {val}");
    }
}

#[test]
fn assertion_empty_string_equals_empty_string() {
    let a = assertion("empty", AssertionCondition::Equals, serde_json::json!(""));
    assert!(evaluate_assertion(serde_json::json!(""), &a).passed);
}

#[test]
fn assertion_very_large_number() {
    let big = serde_json::json!(1e308);
    let a = assertion("big", AssertionCondition::GreaterThan, serde_json::json!(0));
    assert!(evaluate_assertion(big, &a).passed);
}

#[test]
fn assertion_negative_numbers() {
    let a = assertion("neg", AssertionCondition::LessThan, serde_json::json!(0));
    assert!(evaluate_assertion(serde_json::json!(-1), &a).passed);
    assert!(evaluate_assertion(serde_json::json!(-1000000), &a).passed);
}

#[test]
fn assertion_result_has_failure_message() {
    let a = assertion("check", AssertionCondition::Equals, serde_json::json!(1));
    let result = evaluate_assertion(serde_json::json!(2), &a);
    assert!(!result.passed);
    assert!(result.message.is_some());
    let msg = result.message.unwrap();
    assert!(msg.contains("check"));
    assert!(msg.contains("failed"));
}

#[test]
fn assertion_result_no_message_on_pass() {
    let a = assertion("ok", AssertionCondition::Equals, serde_json::json!(1));
    let result = evaluate_assertion(serde_json::json!(1), &a);
    assert!(result.passed);
    assert!(result.message.is_none());
}

#[test]
fn verify_both_null() {
    let result = verify_state(serde_json::Value::Null, serde_json::Value::Null);
    assert!(result.passed);
}

#[test]
fn verify_scalar_root_divergence() {
    let result = verify_state(serde_json::json!(1), serde_json::json!(2));
    assert!(!result.passed);
    assert_eq!(result.divergences[0].path, "$");
}

#[test]
fn verify_array_vs_object() {
    let result = verify_state(serde_json::json!([1, 2]), serde_json::json!({"a": 1}));
    assert!(!result.passed);
    assert_eq!(result.divergences[0].severity, DivergenceSeverity::Error);
}

#[test]
fn assertion_condition_from_str() {
    assert_eq!(
        "equals".parse::<AssertionCondition>().unwrap(),
        AssertionCondition::Equals
    );
    assert_eq!(
        "not_equals".parse::<AssertionCondition>().unwrap(),
        AssertionCondition::NotEquals
    );
    assert_eq!(
        "contains".parse::<AssertionCondition>().unwrap(),
        AssertionCondition::Contains
    );
    assert_eq!(
        "greater_than".parse::<AssertionCondition>().unwrap(),
        AssertionCondition::GreaterThan
    );
    assert_eq!(
        "less_than".parse::<AssertionCondition>().unwrap(),
        AssertionCondition::LessThan
    );
    assert_eq!(
        "truthy".parse::<AssertionCondition>().unwrap(),
        AssertionCondition::Truthy
    );
    assert_eq!(
        "falsy".parse::<AssertionCondition>().unwrap(),
        AssertionCondition::Falsy
    );
    assert_eq!(
        "exists".parse::<AssertionCondition>().unwrap(),
        AssertionCondition::Exists
    );
    assert_eq!(
        "type_is".parse::<AssertionCondition>().unwrap(),
        AssertionCondition::TypeIs
    );
    assert!("bogus".parse::<AssertionCondition>().is_err());
}

#[test]
fn assertion_truthy_empty_array_is_truthy() {
    let a = assertion("t", AssertionCondition::Truthy, serde_json::Value::Null);
    assert!(evaluate_assertion(serde_json::json!([]), &a).passed);
}

#[test]
fn assertion_truthy_empty_object_is_truthy() {
    let a = assertion("t", AssertionCondition::Truthy, serde_json::Value::Null);
    assert!(evaluate_assertion(serde_json::json!({}), &a).passed);
}

// ── Group 6: Type Serialization Round-trips ──────────────────────────────

#[test]
fn window_state_serde_roundtrip() {
    let state = WindowState {
        label: "main".to_string(),
        title: "Test App".to_string(),
        url: "https://example.com".to_string(),
        visible: true,
        focused: false,
        maximized: true,
        minimized: false,
        fullscreen: false,
        position: (100, 200),
        size: (1920, 1080),
    };
    let json = serde_json::to_string(&state).unwrap();
    let rt: WindowState = serde_json::from_str(&json).unwrap();
    assert_eq!(rt, state);
}

#[test]
fn window_state_extreme_values() {
    let state = WindowState {
        label: "".to_string(),
        title: "".to_string(),
        url: "".to_string(),
        visible: false,
        focused: false,
        maximized: false,
        minimized: false,
        fullscreen: false,
        position: (i32::MIN, i32::MAX),
        size: (0, u32::MAX),
    };
    let json = serde_json::to_string(&state).unwrap();
    let rt: WindowState = serde_json::from_str(&json).unwrap();
    assert_eq!(rt.position, (i32::MIN, i32::MAX));
    assert_eq!(rt.size, (0, u32::MAX));
}

#[test]
fn ghost_command_report_serde_roundtrip() {
    let report = GhostCommandReport {
        ghost_commands: vec![
            GhostCommand {
                name: "phantom_cmd".to_string(),
                source: GhostSource::FrontendOnly,
                description: Some("A ghost".to_string()),
            },
            GhostCommand {
                name: "unused_backend".to_string(),
                source: GhostSource::RegistryOnly,
                description: None,
            },
        ],
        total_frontend_commands: 5,
        total_registry_commands: 3,
    };
    let json = serde_json::to_string(&report).unwrap();
    let rt: GhostCommandReport = serde_json::from_str(&json).unwrap();
    assert_eq!(rt, report);
}

#[test]
fn ipc_integrity_report_serde_roundtrip() {
    let report = verification::IpcIntegrityReport {
        total_calls: 100,
        completed: 90,
        pending: 5,
        errored: 5,
        stale_calls: vec![verification::StaleCall {
            id: "s1".to_string(),
            command: "slow_cmd".to_string(),
            timestamp: Utc::now(),
            age_ms: 10000,
            webview_label: "main".to_string(),
        }],
        error_calls: vec![verification::ErrorCall {
            id: "e1".to_string(),
            command: "bad_cmd".to_string(),
            timestamp: Utc::now(),
            error: "boom".to_string(),
            webview_label: "main".to_string(),
        }],
        healthy: false,
    };
    let json = serde_json::to_string(&report).unwrap();
    let rt: verification::IpcIntegrityReport = serde_json::from_str(&json).unwrap();
    assert_eq!(rt.total_calls, 100);
    assert_eq!(rt.stale_calls.len(), 1);
    assert_eq!(rt.error_calls.len(), 1);
    assert!(!rt.healthy);
}

#[test]
fn recorded_session_serde_roundtrip() {
    let session = RecordedSession {
        id: "test-session".to_string(),
        started_at: Utc::now(),
        events: vec![
            RecordedEvent {
                index: 0,
                timestamp: Utc::now(),
                event: ipc("1", "cmd_a"),
            },
            RecordedEvent {
                index: 1,
                timestamp: Utc::now(),
                event: state_change("k"),
            },
        ],
        checkpoints: vec![StateCheckpoint {
            id: "cp1".to_string(),
            label: Some("Midpoint".to_string()),
            timestamp: Utc::now(),
            state: serde_json::json!({"count": 42}),
            event_index: 1,
        }],
    };
    let json = serde_json::to_string(&session).unwrap();
    let rt: RecordedSession = serde_json::from_str(&json).unwrap();
    assert_eq!(rt.id, "test-session");
    assert_eq!(rt.events.len(), 2);
    assert_eq!(rt.checkpoints.len(), 1);
    assert_eq!(rt.checkpoints[0].state, serde_json::json!({"count": 42}));
}

#[test]
fn verification_result_serde_roundtrip() {
    let result = VerificationResult {
        passed: false,
        frontend_state: serde_json::json!({"x": 1}),
        backend_state: serde_json::json!({"x": 2}),
        divergences: vec![Divergence {
            path: "x".to_string(),
            frontend_value: serde_json::json!(1),
            backend_value: serde_json::json!(2),
            severity: DivergenceSeverity::Error,
        }],
    };
    let json = serde_json::to_string(&result).unwrap();
    let rt: VerificationResult = serde_json::from_str(&json).unwrap();
    assert_eq!(rt, result);
}

#[test]
fn semantic_assertion_serde_roundtrip() {
    let a = SemanticAssertion {
        label: "count check".to_string(),
        condition: AssertionCondition::GreaterThan,
        expected: serde_json::json!(10),
    };
    let json = serde_json::to_string(&a).unwrap();
    let rt: SemanticAssertion = serde_json::from_str(&json).unwrap();
    assert_eq!(rt, a);
}

#[test]
fn assertion_result_serde_roundtrip() {
    let result = AssertionResult {
        label: "test".to_string(),
        passed: false,
        actual: serde_json::json!(5),
        expected: serde_json::json!(10),
        message: Some("failed".to_string()),
    };
    let json = serde_json::to_string(&result).unwrap();
    let rt: AssertionResult = serde_json::from_str(&json).unwrap();
    assert_eq!(rt, result);
}

#[test]
fn app_event_ipc_serde_roundtrip() {
    let event = ipc("42", "test_cmd");
    let json = serde_json::to_string(&event).unwrap();
    let rt: AppEvent = serde_json::from_str(&json).unwrap();
    match &rt {
        AppEvent::Ipc(c) => {
            assert_eq!(c.id, "42");
            assert_eq!(c.command, "test_cmd");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn app_event_all_variants_serde() {
    let events = vec![
        ipc("1", "cmd"),
        state_change("key"),
        dom_mutation(10),
        window_event("main", "close"),
        dom_interaction(InteractionKind::DoubleClick, "#el"),
    ];
    for event in events {
        let json = serde_json::to_string(&event).unwrap();
        let rt: AppEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(rt, event);
    }
}

#[test]
fn ref_handle_serde_roundtrip() {
    let handle = RefHandle {
        id: "e5".to_string(),
        selector: "button.submit".to_string(),
        role: Some("button".to_string()),
        name: Some("Submit".to_string()),
    };
    let json = serde_json::to_string(&handle).unwrap();
    let rt: RefHandle = serde_json::from_str(&json).unwrap();
    assert_eq!(rt, handle);
}

#[test]
fn memory_delta_serde_roundtrip() {
    let delta = types::MemoryDelta {
        before_bytes: 1024,
        after_bytes: 2048,
        delta_bytes: 1024,
        command: "allocate".to_string(),
    };
    let json = serde_json::to_string(&delta).unwrap();
    let rt: types::MemoryDelta = serde_json::from_str(&json).unwrap();
    assert_eq!(rt, delta);
}

// ── Group 7: Concurrent Safety ───────────────────────────────────────────

#[test]
fn concurrent_eventlog_push_from_10_threads() {
    let log = Arc::new(EventLog::new(5000));
    let mut handles = Vec::new();
    for t in 0..10 {
        let log = Arc::clone(&log);
        handles.push(thread::spawn(move || {
            for i in 0..500 {
                log.push(ipc(&format!("{t}-{i}"), &format!("t{t}")));
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(log.len(), 5000);
}

#[test]
fn concurrent_eventlog_push_and_snapshot() {
    let log = Arc::new(EventLog::new(1000));
    let log_writer = Arc::clone(&log);
    let writer = thread::spawn(move || {
        for i in 0..1000 {
            log_writer.push(ipc(&i.to_string(), "write"));
        }
    });
    let log_reader = Arc::clone(&log);
    let reader = thread::spawn(move || {
        let mut max_len = 0;
        for _ in 0..100 {
            let snap = log_reader.snapshot();
            if snap.len() > max_len {
                max_len = snap.len();
            }
        }
        max_len
    });
    writer.join().unwrap();
    let _max = reader.join().unwrap();
    assert_eq!(log.len(), 1000);
}

#[test]
fn concurrent_eventlog_push_exceeds_capacity() {
    let log = Arc::new(EventLog::new(100));
    let mut handles = Vec::new();
    for t in 0..20 {
        let log = Arc::clone(&log);
        handles.push(thread::spawn(move || {
            for i in 0..100 {
                log.push(ipc(&format!("{t}-{i}"), "spam"));
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(log.len(), 100);
}

#[test]
fn concurrent_registry_register_and_search() {
    let reg = Arc::new(CommandRegistry::new());
    let mut handles = Vec::new();
    for t in 0..10 {
        let reg = Arc::clone(&reg);
        handles.push(thread::spawn(move || {
            for i in 0..50 {
                reg.register(
                    CommandInfo::new(format!("cmd_{t}_{i}"))
                        .with_description(format!("Thread {t} cmd {i}")),
                );
            }
        }));
    }
    for t in 0..5 {
        let reg = Arc::clone(&reg);
        handles.push(thread::spawn(move || {
            for _ in 0..50 {
                let _ = reg.search(&format!("cmd_{t}"));
                let _ = reg.resolve(&format!("thread {t}"));
                let _ = reg.list();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(reg.count(), 500);
}

#[test]
fn concurrent_registry_count_during_writes() {
    let reg = Arc::new(CommandRegistry::new());
    let reg_writer = Arc::clone(&reg);
    let writer = thread::spawn(move || {
        for i in 0..1000 {
            reg_writer.register(CommandInfo::new(format!("cmd_{i}")));
        }
    });
    let reg_reader = Arc::clone(&reg);
    let reader = thread::spawn(move || {
        let mut counts = Vec::new();
        for _ in 0..100 {
            counts.push(reg_reader.count());
        }
        counts
    });
    writer.join().unwrap();
    let counts = reader.join().unwrap();
    assert_eq!(reg.count(), 1000);
    for pair in counts.windows(2) {
        assert!(pair[1] >= pair[0]);
    }
}

#[test]
fn concurrent_recorder_record_events() {
    let rec = Arc::new(EventRecorder::new(10_000));
    rec.start("concurrent".to_string()).unwrap();
    let mut handles = Vec::new();
    for t in 0..10 {
        let r = Arc::clone(&rec);
        handles.push(thread::spawn(move || {
            for i in 0..100 {
                r.record_event(ipc(&format!("{t}-{i}"), &format!("t{t}")));
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
fn concurrent_recorder_checkpoints() {
    let rec = Arc::new(EventRecorder::new(10_000));
    rec.start("cp".to_string()).unwrap();
    let mut handles = Vec::new();
    for t in 0..5 {
        let r = Arc::clone(&rec);
        handles.push(thread::spawn(move || {
            for i in 0..20 {
                r.checkpoint(
                    format!("cp-{t}-{i}"),
                    None,
                    serde_json::json!({"t": t, "i": i}),
                )
                .unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(rec.checkpoint_count(), 100);
}

#[test]
fn concurrent_recorder_events_and_checkpoints_interleaved() {
    let rec = Arc::new(EventRecorder::new(10_000));
    rec.start("mixed".to_string()).unwrap();
    let mut handles = Vec::new();
    for t in 0..5 {
        let r = Arc::clone(&rec);
        handles.push(thread::spawn(move || {
            for i in 0..100 {
                r.record_event(ipc(&format!("{t}-{i}"), "cmd"));
                if i % 10 == 0 {
                    let _ = r.checkpoint(format!("cp-{t}-{i}"), None, serde_json::json!(null));
                }
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(rec.event_count(), 500);
    let cp_count = rec.checkpoint_count();
    assert_eq!(cp_count, 5 * 10);
}

#[test]
fn concurrent_eventlog_clear_during_push() {
    let log = Arc::new(EventLog::new(100));
    let log_writer = Arc::clone(&log);
    let writer = thread::spawn(move || {
        for i in 0..1000 {
            log_writer.push(ipc(&i.to_string(), "cmd"));
        }
    });
    let log_clearer = Arc::clone(&log);
    let clearer = thread::spawn(move || {
        for _ in 0..50 {
            log_clearer.clear();
        }
    });
    writer.join().unwrap();
    clearer.join().unwrap();
    assert!(log.len() <= 100);
}

// ── Group: Display trait coverage ────────────────────────────────────────

#[test]
fn display_verification_result_passed() {
    let result = VerificationResult {
        passed: true,
        frontend_state: serde_json::json!({}),
        backend_state: serde_json::json!({}),
        divergences: vec![],
    };
    assert_eq!(result.to_string(), "verification passed");
}

#[test]
fn display_verification_result_failed() {
    let result = VerificationResult {
        passed: false,
        frontend_state: serde_json::json!({}),
        backend_state: serde_json::json!({}),
        divergences: vec![Divergence {
            path: "x".to_string(),
            frontend_value: serde_json::json!(1),
            backend_value: serde_json::json!(2),
            severity: DivergenceSeverity::Error,
        }],
    };
    assert_eq!(result.to_string(), "verification failed: 1 divergence(s)");
}

#[test]
fn display_ghost_command_report() {
    let report = GhostCommandReport {
        ghost_commands: vec![GhostCommand {
            name: "phantom".to_string(),
            source: GhostSource::FrontendOnly,
            description: None,
        }],
        total_frontend_commands: 2,
        total_registry_commands: 1,
    };
    assert_eq!(
        report.to_string(),
        "1 ghost command(s) (2 frontend, 1 registry)"
    );
}

#[test]
fn display_ipc_integrity_healthy() {
    let report = verification::IpcIntegrityReport {
        total_calls: 10,
        completed: 10,
        pending: 0,
        errored: 0,
        stale_calls: vec![],
        error_calls: vec![],
        healthy: true,
    };
    assert_eq!(report.to_string(), "IPC healthy: 10/10 completed");
}

#[test]
fn display_ipc_integrity_unhealthy() {
    let report = verification::IpcIntegrityReport {
        total_calls: 10,
        completed: 7,
        pending: 2,
        errored: 1,
        stale_calls: vec![],
        error_calls: vec![],
        healthy: false,
    };
    assert!(report.to_string().contains("unhealthy"));
}

#[test]
fn display_divergence_severity() {
    assert_eq!(DivergenceSeverity::Info.to_string(), "info");
    assert_eq!(DivergenceSeverity::Warning.to_string(), "warning");
    assert_eq!(DivergenceSeverity::Error.to_string(), "error");
}

#[test]
fn display_ghost_source() {
    assert_eq!(GhostSource::FrontendOnly.to_string(), "frontend-only");
    assert_eq!(GhostSource::RegistryOnly.to_string(), "registry-only");
}

#[test]
fn display_ipc_result() {
    assert_eq!(IpcResult::Pending.to_string(), "pending");
    assert_eq!(IpcResult::Ok(serde_json::json!(1)).to_string(), "ok");
    assert_eq!(
        IpcResult::Err("fail".to_string()).to_string(),
        "error: fail"
    );
}

#[test]
fn display_ipc_call() {
    let call = IpcCall {
        id: "42".to_string(),
        command: "save".to_string(),
        timestamp: Utc::now(),
        duration_ms: Some(5),
        result: IpcResult::Ok(serde_json::json!("ok")),
        arg_size_bytes: 10,
        webview_label: "main".to_string(),
    };
    let s = call.to_string();
    assert!(s.contains("save"));
    assert!(s.contains("[42]"));
}

#[test]
fn display_scored_command() {
    let sc = ScoredCommand {
        command: CommandInfo::new("test_cmd"),
        score: 5.75,
    };
    let s = sc.to_string();
    assert!(s.contains("test_cmd"));
    assert!(s.contains("5.75"));
}

#[test]
fn display_assertion_condition() {
    assert_eq!(AssertionCondition::Equals.to_string(), "equals");
    assert_eq!(AssertionCondition::NotEquals.to_string(), "not_equals");
    assert_eq!(AssertionCondition::Contains.to_string(), "contains");
    assert_eq!(AssertionCondition::GreaterThan.to_string(), "greater_than");
    assert_eq!(AssertionCondition::LessThan.to_string(), "less_than");
    assert_eq!(AssertionCondition::Truthy.to_string(), "truthy");
    assert_eq!(AssertionCondition::Falsy.to_string(), "falsy");
    assert_eq!(AssertionCondition::Exists.to_string(), "exists");
    assert_eq!(AssertionCondition::TypeIs.to_string(), "type_is");
}

#[test]
fn display_interaction_kind() {
    assert_eq!(InteractionKind::Click.to_string(), "click");
    assert_eq!(InteractionKind::DoubleClick.to_string(), "double_click");
    assert_eq!(InteractionKind::Fill.to_string(), "fill");
    assert_eq!(InteractionKind::KeyPress.to_string(), "key_press");
    assert_eq!(InteractionKind::Select.to_string(), "select");
    assert_eq!(InteractionKind::Navigate.to_string(), "navigate");
    assert_eq!(InteractionKind::Scroll.to_string(), "scroll");
}

// ── Group: IPC Integrity edge cases ──────────────────────────────────────

#[test]
fn ipc_integrity_all_pending_within_threshold() {
    let log = EventLog::new(100);
    for i in 0..5 {
        log.push(ipc_pending(&i.to_string(), &format!("cmd_{i}")));
    }
    let report = check_ipc_integrity(&log, 60_000);
    assert!(report.healthy);
    assert_eq!(report.pending, 5);
    assert_eq!(report.completed, 0);
    assert!(report.stale_calls.is_empty());
}

#[test]
fn ipc_integrity_all_errors() {
    let log = EventLog::new(100);
    for i in 0..5 {
        log.push(ipc_err(&i.to_string(), &format!("cmd_{i}"), "broken"));
    }
    let report = check_ipc_integrity(&log, 5000);
    assert!(!report.healthy);
    assert_eq!(report.errored, 5);
    assert_eq!(report.error_calls.len(), 5);
}

#[test]
fn ipc_integrity_mixed_results() {
    let log = EventLog::new(100);
    log.push(ipc("1", "ok_cmd"));
    log.push(ipc_pending("2", "pending_cmd"));
    log.push(ipc_err("3", "err_cmd", "oops"));
    let report = check_ipc_integrity(&log, 60_000);
    assert!(!report.healthy);
    assert_eq!(report.total_calls, 3);
    assert_eq!(report.completed, 1);
    assert_eq!(report.pending, 1);
    assert_eq!(report.errored, 1);
}

#[test]
fn ipc_integrity_ignores_non_ipc_events() {
    let log = EventLog::new(100);
    log.push(state_change("k"));
    log.push(dom_mutation(1));
    log.push(window_event("main", "focus"));
    let report = check_ipc_integrity(&log, 5000);
    assert!(report.healthy);
    assert_eq!(report.total_calls, 0);
}

// ── Group: Ghost command edge cases ──────────────────────────────────────

#[test]
fn ghost_commands_many_frontend_only() {
    let reg = CommandRegistry::new();
    let frontend: Vec<String> = (0..100).map(|i| format!("cmd_{i}")).collect();
    let report = detect_ghost_commands(&frontend, &reg);
    assert_eq!(report.ghost_commands.len(), 100);
    assert!(
        report
            .ghost_commands
            .iter()
            .all(|g| g.source == GhostSource::FrontendOnly)
    );
}

#[test]
fn ghost_commands_many_registry_only() {
    let reg = CommandRegistry::new();
    for i in 0..100 {
        reg.register(CommandInfo::new(format!("cmd_{i}")));
    }
    let report = detect_ghost_commands(&[], &reg);
    assert_eq!(report.ghost_commands.len(), 100);
    assert!(
        report
            .ghost_commands
            .iter()
            .all(|g| g.source == GhostSource::RegistryOnly)
    );
}

#[test]
fn ghost_commands_sorted_by_name() {
    let reg = CommandRegistry::new();
    reg.register(CommandInfo::new("zebra"));
    reg.register(CommandInfo::new("alpha"));
    let frontend = vec!["mango".to_string(), "banana".to_string()];
    let report = detect_ghost_commands(&frontend, &reg);
    let names: Vec<&str> = report
        .ghost_commands
        .iter()
        .map(|g| g.name.as_str())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
}

// ── Group: Error type coverage ───────────────────────────────────────────

#[test]
fn error_display_messages() {
    let errors = vec![
        VictauriError::capacity_exceeded("too many"),
        VictauriError::invalid_input("bad value"),
        VictauriError::NoActiveRecording,
        VictauriError::RecordingAlreadyActive,
        VictauriError::CheckpointNotFound {
            id: "cp99".to_string(),
        },
        VictauriError::CommandNotFound {
            name: "missing".to_string(),
        },
        VictauriError::InvalidRefHandle {
            ref_id: "e999".to_string(),
        },
        VictauriError::UnknownCondition {
            condition: "bogus".to_string(),
        },
    ];
    for err in &errors {
        let msg = err.to_string();
        assert!(!msg.is_empty());
    }
    assert!(errors[0].to_string().contains("too many"));
    assert!(errors[4].to_string().contains("cp99"));
    assert!(errors[5].to_string().contains("missing"));
}

#[test]
fn ipc_call_into_app_event() {
    let call = IpcCall {
        id: "1".to_string(),
        command: "test".to_string(),
        timestamp: Utc::now(),
        duration_ms: None,
        result: IpcResult::Pending,
        arg_size_bytes: 0,
        webview_label: "main".to_string(),
    };
    let event: AppEvent = call.into();
    assert!(matches!(event, AppEvent::Ipc(_)));
}

#[test]
fn app_event_timestamp_all_variants() {
    let ts = Utc::now();
    let events = vec![
        AppEvent::Ipc(IpcCall {
            id: "1".to_string(),
            command: "cmd".to_string(),
            timestamp: ts,
            duration_ms: None,
            result: IpcResult::Pending,
            arg_size_bytes: 0,
            webview_label: "main".to_string(),
        }),
        AppEvent::StateChange {
            key: "k".to_string(),
            timestamp: ts,
            caused_by: None,
        },
        AppEvent::DomMutation {
            webview_label: "main".to_string(),
            timestamp: ts,
            mutation_count: 1,
        },
        AppEvent::DomInteraction {
            action: InteractionKind::Click,
            selector: "#x".to_string(),
            value: None,
            timestamp: ts,
            webview_label: "main".to_string(),
        },
        AppEvent::WindowEvent {
            label: "main".to_string(),
            event: "focus".to_string(),
            timestamp: ts,
        },
    ];
    for event in &events {
        assert_eq!(event.timestamp(), ts);
    }
}

#[test]
fn verify_state_deep_nesting_is_depth_bounded() {
    // verify_state takes a fully caller-controlled backend_state; deep nesting must
    // be depth-bounded, not an unbounded recursion that stack-overflows the host
    // (audit #14).
    fn nest(leaf: serde_json::Value, n: usize) -> serde_json::Value {
        let mut v = leaf;
        for _ in 0..n {
            v = serde_json::json!({ "a": v });
        }
        v
    }
    // 600 levels > the 128 compare cap (so the guard fires) but is safe to drop.
    let f = nest(serde_json::json!(1), 600);
    let b = nest(serde_json::json!(2), 600);
    let result = verify_state(f, b); // guard prevents recursing past the cap
    assert!(!result.passed);
    assert!(
        result.divergences.iter().any(|d| d
            .frontend_value
            .as_str()
            .is_some_and(|s| s.contains("max compare depth"))),
        "expected a depth-bound divergence"
    );
}

#[test]
fn recorder_import_clamps_oversized_session() {
    // An imported session is fully attacker-controlled: it must be clamped to the
    // configured caps rather than loaded verbatim (audit #19).
    let small = EventRecorder::new(10);
    let big = EventRecorder::new(10_000);
    big.start("src".to_string()).unwrap();
    for i in 0..500 {
        big.record_event(ipc(&format!("e{i}"), "cmd"));
    }
    let session = big.export().expect("active session");
    assert_eq!(session.events.len(), 500);

    small.import(session); // clamp to small.max_events
    let clamped = small.export().expect("active after import");
    assert!(
        clamped.events.len() <= 10,
        "import must clamp to max_events, got {}",
        clamped.events.len()
    );
}

#[test]
fn resolve_caps_pathological_query() {
    // A multi-MB query must not be a CPU DoS — resolve() bounds the query length
    // before scoring (audit #20).
    let reg = CommandRegistry::new();
    reg.register(CommandInfo::new("get_settings").with_description("get the settings"));
    let huge = "get settings ".repeat(500_000); // ~6.5 MB
    let results = reg.resolve(&huge); // returns promptly thanks to the length cap
    assert!(!results.is_empty());
}
