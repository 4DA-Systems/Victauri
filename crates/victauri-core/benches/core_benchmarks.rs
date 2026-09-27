use chrono::Utc;
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use victauri_core::event::{AppEvent, EventLog, IpcCall, IpcResult};
use victauri_core::recording::EventRecorder;
use victauri_core::registry::{CommandInfo, CommandRegistry};
use victauri_core::verification;

fn make_ipc(id: &str, cmd: &str) -> AppEvent {
    AppEvent::Ipc(IpcCall::new(
        id.to_string(),
        cmd.to_string(),
        Utc::now(),
        IpcResult::Ok(serde_json::json!("ok")),
        Some(1),
        32,
        "main".to_string(),
    ))
}

fn bench_event_log(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_log");

    group.bench_function("push_10k", |b| {
        let log = EventLog::new(10_000);
        b.iter(|| {
            for i in 0..10_000 {
                log.push(make_ipc(&i.to_string(), "bench_cmd"));
            }
        });
    });

    group.bench_function("push_with_eviction", |b| {
        let log = EventLog::new(1_000);
        let mut i = 0u64;
        b.iter(|| {
            log.push(make_ipc(&i.to_string(), "bench_cmd"));
            i += 1;
        });
    });

    group.bench_function("snapshot_1k", |b| {
        let log = EventLog::new(1_000);
        for i in 0..1_000 {
            log.push(make_ipc(&i.to_string(), "cmd"));
        }
        b.iter(|| {
            black_box(log.snapshot());
        });
    });

    group.bench_function("ipc_calls_filter_1k", |b| {
        let log = EventLog::new(1_000);
        for i in 0..500 {
            log.push(make_ipc(&i.to_string(), "ipc_cmd"));
        }
        for i in 0..500 {
            log.push(AppEvent::state_change(format!("key_{i}"), Utc::now(), None));
        }
        b.iter(|| {
            black_box(log.ipc_calls());
        });
    });

    group.finish();
}

fn bench_registry(c: &mut Criterion) {
    let mut group = c.benchmark_group("registry");

    group.bench_function("register_100", |b| {
        b.iter(|| {
            let reg = CommandRegistry::new();
            for i in 0..100 {
                let mut cmd = CommandInfo::new(format!("cmd_{i}"))
                    .with_description(format!("Description for command {i}"))
                    .with_intent(format!("intent_{i}"))
                    .with_category("test");
                cmd.examples = vec![format!("example {i}")];
                reg.register(cmd);
            }
        });
    });

    group.bench_function("search_100_commands", |b| {
        let reg = CommandRegistry::new();
        for i in 0..100 {
            reg.register(
                CommandInfo::new(format!("cmd_{i}"))
                    .with_description(format!("Description for command {i}")),
            );
        }
        b.iter(|| {
            black_box(reg.search("cmd_5"));
        });
    });

    group.bench_function("resolve_100_commands", |b| {
        let reg = CommandRegistry::new();
        for i in 0..100 {
            reg.register(
                CommandInfo::new(format!("get_user_{i}"))
                    .with_description(format!("Fetch user data for user {i}"))
                    .with_intent("read user data")
                    .with_category("users"),
            );
        }
        b.iter(|| {
            black_box(reg.resolve("get user info"));
        });
    });

    group.finish();
}

fn bench_verification(c: &mut Criterion) {
    let mut group = c.benchmark_group("verification");

    group.bench_function("verify_flat_10_keys", |b| {
        let frontend = serde_json::json!({
            "a": 1, "b": "hello", "c": true, "d": null,
            "e": 42.5, "f": [1,2,3], "g": "world", "h": false,
            "i": 100, "j": "test"
        });
        let backend = frontend.clone();
        b.iter(|| {
            black_box(victauri_core::verify_state(
                frontend.clone(),
                backend.clone(),
            ));
        });
    });

    group.bench_function("verify_nested_5_deep", |b| {
        let frontend = serde_json::json!({
            "a": { "b": { "c": { "d": { "e": 42 } } } }
        });
        let backend = frontend.clone();
        b.iter(|| {
            black_box(victauri_core::verify_state(
                frontend.clone(),
                backend.clone(),
            ));
        });
    });

    group.bench_function("verify_with_divergences", |b| {
        let frontend = serde_json::json!({
            "count": 5, "name": "alice", "active": true,
            "tags": ["a", "b", "c"], "meta": { "version": 2 }
        });
        let backend = serde_json::json!({
            "count": 6, "name": "alice", "active": false,
            "tags": ["a", "b"], "meta": { "version": 3 }
        });
        b.iter(|| {
            black_box(victauri_core::verify_state(
                frontend.clone(),
                backend.clone(),
            ));
        });
    });

    group.bench_function("evaluate_assertion", |b| {
        let assertion = verification::SemanticAssertion::new(
            "check_count".to_string(),
            verification::AssertionCondition::GreaterThan,
            serde_json::json!(10),
        );
        b.iter(|| {
            black_box(verification::evaluate_assertion(
                serde_json::json!(42),
                &assertion,
            ));
        });
    });

    group.finish();
}

fn bench_recording(c: &mut Criterion) {
    let mut group = c.benchmark_group("recording");

    group.bench_function("record_1k_events", |b| {
        b.iter(|| {
            let rec = EventRecorder::new(10_000);
            rec.start("bench".to_string()).unwrap();
            for i in 0..1_000 {
                rec.record_event(make_ipc(&i.to_string(), "bench_cmd"));
            }
            black_box(rec.stop());
        });
    });

    group.bench_function("record_with_eviction", |b| {
        let rec = EventRecorder::new(100);
        rec.start("bench".to_string()).unwrap();
        let mut i = 0u64;
        b.iter(|| {
            rec.record_event(make_ipc(&i.to_string(), "bench_cmd"));
            i += 1;
        });
    });

    group.bench_function("checkpoint_100", |b| {
        b.iter(|| {
            let rec = EventRecorder::new(10_000);
            rec.start("bench".to_string()).unwrap();
            for i in 0..100 {
                rec.checkpoint(
                    format!("cp_{i}"),
                    Some(format!("Checkpoint {i}")),
                    serde_json::json!({"step": i}),
                )
                .unwrap();
            }
            black_box(rec.stop());
        });
    });

    group.bench_function("events_since_in_1k", |b| {
        let rec = EventRecorder::new(10_000);
        rec.start("bench".to_string()).unwrap();
        for i in 0..1_000 {
            rec.record_event(make_ipc(&i.to_string(), "cmd"));
        }
        b.iter(|| {
            black_box(rec.events_since(500));
        });
    });

    group.finish();
}

fn bench_ghost_commands(c: &mut Criterion) {
    let mut group = c.benchmark_group("ghost_commands");

    group.bench_function("detect_100_commands", |b| {
        let reg = CommandRegistry::new();
        for i in 0..100 {
            reg.register(CommandInfo::new(format!("backend_cmd_{i}")));
        }
        let frontend: Vec<String> = (0..100).map(|i| format!("frontend_cmd_{i}")).collect();
        b.iter(|| {
            black_box(victauri_core::detect_ghost_commands(&frontend, &reg));
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_event_log,
    bench_registry,
    bench_verification,
    bench_recording,
    bench_ghost_commands,
);
criterion_main!(benches);
