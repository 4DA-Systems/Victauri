use super::*;
use tracing_subscriber::layer::SubscriberExt;

fn entry(level: LogLevel, target: &str, message: &str) -> BackendLogEntry {
    BackendLogEntry {
        seq: 0,
        ts_ms: now_ms(),
        level,
        target: target.to_string(),
        message: message.to_string(),
        fields: serde_json::Map::new(),
        spans: Vec::new(),
        thread: None,
        location: None,
        source: LogSource::Tracing,
    }
}

/// Run `f` with a subscriber whose only layer is a capture layer on `buf`.
fn with_capture(buf: &Arc<LogBuffer>, f: impl FnOnce()) {
    let subscriber =
        tracing_subscriber::registry().with(BackendLogLayer::with_buffer(Arc::clone(buf)));
    tracing::subscriber::with_default(subscriber, f);
}

#[test]
fn sequence_numbers_are_monotonic_and_ring_evicts_oldest() {
    let buf = LogBuffer::new(3, usize::MAX);
    for i in 0..5 {
        buf.push(entry(LogLevel::Info, "t", &format!("m{i}")));
    }
    let page = buf.query(&LogQuery {
        limit: 10,
        ..LogQuery::default()
    });
    let seqs: Vec<u64> = page.entries.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![2, 3, 4], "only the newest 3 survive, in order");
    assert_eq!(page.next_seq, 5);
    assert_eq!(page.oldest_seq, 2);
    assert_eq!(page.evicted, 2);
}

#[test]
fn byte_budget_evicts_even_below_entry_capacity() {
    let buf = LogBuffer::new(1000, 2_000);
    for _ in 0..50 {
        buf.push(entry(LogLevel::Info, "t", &"x".repeat(400)));
    }
    let page = buf.query(&LogQuery {
        limit: 1000,
        ..LogQuery::default()
    });
    assert!(
        page.entries.len() < 10,
        "byte cap bounds memory: {}",
        page.entries.len()
    );
    assert!(!page.entries.is_empty());
}

#[test]
fn query_filters_by_level_target_text_and_cursor() {
    let buf = LogBuffer::default();
    buf.push(entry(LogLevel::Debug, "app::db", "opened pool"));
    buf.push(entry(LogLevel::Warn, "app::scoring", "context build slow"));
    buf.push(entry(LogLevel::Error, "app::db", "query FAILED"));
    buf.push(entry(LogLevel::Info, "hyper::client", "connected"));

    let warn_up = buf.query(&LogQuery {
        min_level: LogLevel::Warn,
        ..LogQuery::default()
    });
    assert_eq!(warn_up.entries.len(), 2);

    let db = buf.query(&LogQuery {
        targets: vec!["app::db".into()],
        ..LogQuery::default()
    });
    assert_eq!(db.entries.len(), 2);

    let text = buf.query(&LogQuery {
        contains: Some("failed".into()),
        ..LogQuery::default()
    });
    assert_eq!(text.entries.len(), 1, "substring match is case-insensitive");
    assert_eq!(text.entries[0].message, "query FAILED");

    let after = buf.query(&LogQuery {
        since_seq: Some(2),
        ..LogQuery::default()
    });
    assert_eq!(
        after.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert!(!after.gap);
}

#[test]
fn limit_keeps_newest_matches_and_reports_truncation() {
    let buf = LogBuffer::default();
    for i in 0..10 {
        buf.push(entry(LogLevel::Info, "t", &format!("n{i}")));
    }
    let page = buf.query(&LogQuery {
        limit: 3,
        ..LogQuery::default()
    });
    assert!(page.truncated);
    assert_eq!(page.total_matched, 10);
    assert_eq!(
        page.entries
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>(),
        vec!["n7", "n8", "n9"]
    );
}

#[test]
fn stale_cursor_reports_gap() {
    let buf = LogBuffer::new(2, usize::MAX);
    for _ in 0..6 {
        buf.push(entry(LogLevel::Info, "t", "m"));
    }
    let page = buf.query(&LogQuery {
        since_seq: Some(1),
        ..LogQuery::default()
    });
    assert!(page.gap, "entries 1..4 were evicted unseen");
}

#[test]
fn layer_captures_message_typed_fields_spans_and_location() {
    let buf = Arc::new(LogBuffer::default());
    with_capture(&buf, || {
        let span = tracing::info_span!("scoring", run_type = "background_deep");
        let _g = span.enter();
        tracing::warn!(
            target: "4da::scoring",
            caller = "backfill_unscored",
            elapsed_ms = 41_191_u64,
            ok = false,
            "Scoring context build exceeded the 30s soft ceiling"
        );
    });
    let page = buf.query(&LogQuery::default());
    assert_eq!(page.entries.len(), 1);
    let e = &page.entries[0];
    assert_eq!(e.level, LogLevel::Warn);
    assert_eq!(e.target, "4da::scoring");
    assert_eq!(
        e.message,
        "Scoring context build exceeded the 30s soft ceiling"
    );
    assert_eq!(e.fields["caller"], "backfill_unscored");
    assert_eq!(e.fields["elapsed_ms"], 41_191);
    assert_eq!(e.fields["ok"], false);
    assert_eq!(
        e.spans,
        vec![r#"scoring{run_type="background_deep"}"#.to_string()]
    );
    assert!(
        e.location
            .as_deref()
            .is_some_and(|l| l.contains("backend_logs_tests.rs"))
    );
    assert_eq!(e.source, LogSource::Tracing);
}

#[test]
fn layer_records_late_span_fields() {
    let buf = Arc::new(LogBuffer::default());
    with_capture(&buf, || {
        let span = tracing::info_span!("job", id = tracing::field::Empty);
        span.record("id", 7_u64);
        let _g = span.enter();
        tracing::info!(target: "app", "inside");
    });
    let e = &buf.query(&LogQuery::default()).entries[0];
    assert_eq!(e.spans, vec!["job{id=7}".to_string()]);
}

#[test]
fn layer_formats_error_fields_with_their_source_chain() {
    #[derive(Debug)]
    struct Outer(std::io::Error);
    impl std::fmt::Display for Outer {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("load failed")
        }
    }
    impl std::error::Error for Outer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }
    let buf = Arc::new(LogBuffer::default());
    with_capture(&buf, || {
        let err = Outer(std::io::Error::other("disk gone"));
        let err_ref: &(dyn std::error::Error + 'static) = &err;
        tracing::error!(target: "app", error = err_ref, "boom");
    });
    let e = &buf.query(&LogQuery::default()).entries[0];
    assert_eq!(e.fields["error"], "load failed: disk gone");
}

#[test]
fn log_adapter_captures_and_forwards() {
    struct Counting(Arc<std::sync::atomic::AtomicUsize>);
    impl log::Log for Counting {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, _: &log::Record<'_>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        fn flush(&self) {}
    }
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let buf = Arc::new(LogBuffer::default());
    let logger = VictauriLogger::with_buffer(
        Arc::clone(&buf),
        Some(Box::new(Counting(Arc::clone(&hits)))),
    );
    log::Log::log(
        &logger,
        &log::Record::builder()
            .level(log::Level::Warn)
            .target("tauri_plugin_log_app")
            .args(format_args!("disk {} low", 93))
            .file(Some("src/main.rs"))
            .line(Some(12))
            .build(),
    );
    let e = &buf.query(&LogQuery::default()).entries[0];
    assert_eq!(e.level, LogLevel::Warn);
    assert_eq!(e.message, "disk 93 low");
    assert_eq!(e.target, "tauri_plugin_log_app");
    assert_eq!(e.location.as_deref(), Some("src/main.rs:12"));
    assert_eq!(e.source, LogSource::Log);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "the app's own logger still runs"
    );
}

#[test]
fn digest_groups_repeats_counts_levels_and_lists_panics() {
    let buf = LogBuffer::default();
    for cpu in [55, 70, 61, 77] {
        buf.push(entry(
            LogLevel::Info,
            "4da::gate",
            &format!("Scheduler gate policy changed cpu=Some({cpu}.27)"),
        ));
    }
    buf.push(entry(
        LogLevel::Warn,
        "4da::scoring",
        "context build slow elapsed_ms=41191",
    ));
    let mut p = entry(LogLevel::Error, "panic", "index out of bounds");
    p.source = LogSource::Panic;
    buf.push(p);

    let d = buf.digest(10, 5);
    assert_eq!(d.buffered, 6);
    assert_eq!(d.buffered_counts["info"], 4);
    assert_eq!(d.lifetime_counts["error"], 1);
    assert_eq!(d.top_targets[0].target, "4da::gate");
    assert_eq!(
        d.repeated.len(),
        1,
        "the 4 gate lines collapse to one template"
    );
    assert_eq!(d.repeated[0].count, 4);
    assert_eq!(
        d.repeated[0].template,
        "Scheduler gate policy changed cpu=Some(#)"
    );
    assert_eq!(d.recent_problems.len(), 2);
    assert_eq!(d.panics.len(), 1);
}

#[test]
fn message_template_collapses_variable_parts() {
    assert_eq!(
        message_template(r#"Fetched 48 stories from "hn" in 1.5s id=3f2a-9c"#),
        r#"Fetched # stories from "…" in # id=#"#
    );
    assert_eq!(message_template("no numbers here"), "no numbers here");
}

#[tokio::test]
async fn wait_for_wakes_on_a_matching_entry_pushed_later() {
    let buf = Arc::new(LogBuffer::default());
    buf.push(entry(LogLevel::Info, "t", "unrelated"));
    let from = buf.next_seq();
    let producer = Arc::clone(&buf);
    let handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        producer.push(entry(LogLevel::Info, "t", "still unrelated"));
        producer.push(entry(LogLevel::Info, "t", "pipeline complete"));
    });
    let start = std::time::Instant::now();
    let hit = buf
        .wait_for(from, std::time::Duration::from_secs(5), |e| {
            e.message.contains("complete")
        })
        .await;
    handle.await.unwrap();
    assert_eq!(
        hit.map(|e| e.message),
        Some("pipeline complete".to_string())
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
}

#[tokio::test]
async fn wait_for_ignores_entries_before_the_cursor_and_times_out() {
    let buf = LogBuffer::default();
    buf.push(entry(LogLevel::Info, "t", "pipeline complete"));
    let from = buf.next_seq();
    let hit = buf
        .wait_for(from, std::time::Duration::from_millis(100), |e| {
            e.message.contains("complete")
        })
        .await;
    assert!(
        hit.is_none(),
        "an entry before the cursor must not satisfy the wait"
    );
}

#[test]
fn seq_at_or_after_finds_the_first_entry_in_a_time_window() {
    let buf = LogBuffer::default();
    let mut old = entry(LogLevel::Info, "t", "old");
    old.ts_ms = 1_000;
    buf.push(old);
    let mut new = entry(LogLevel::Info, "t", "new");
    new.ts_ms = 5_000;
    buf.push(new);
    assert_eq!(buf.seq_at_or_after(2_000), 1);
    assert_eq!(buf.seq_at_or_after(9_000), 2, "none -> next_seq");
}

#[test]
fn range_returns_the_window_newest_capped() {
    let buf = LogBuffer::default();
    for i in 0..6 {
        let lvl = if i % 2 == 0 {
            LogLevel::Debug
        } else {
            LogLevel::Warn
        };
        buf.push(entry(lvl, "t", &format!("m{i}")));
    }
    let (got, total) = buf.range(1, 6, LogLevel::Warn, 2);
    assert_eq!(total, 3);
    assert_eq!(
        got.iter().map(|e| e.message.as_str()).collect::<Vec<_>>(),
        vec!["m3", "m5"]
    );
}

#[test]
fn long_messages_are_truncated_on_a_char_boundary() {
    let s = truncate_string("é".repeat(10), 5);
    assert!(s.starts_with("éé"));
    assert!(s.contains("bytes]"));
}

#[test]
fn infer_level_reads_common_console_formats() {
    assert_eq!(
        infer_level("2026-09-27T10:32:10.161705Z  WARN 4da::scoring: slow"),
        Some(LogLevel::Warn)
    );
    assert_eq!(
        infer_level("[2026-09-27T10:32:10Z ERROR app] failed"),
        Some(LogLevel::Error)
    );
    assert_eq!(
        infer_level("thread 'main' panicked at src/main.rs:3:5:"),
        Some(LogLevel::Error)
    );
    assert_eq!(
        infer_level("error[E0425]: cannot find value"),
        Some(LogLevel::Error)
    );
    assert_eq!(infer_level("just a line mentioning error later on"), None);
}

#[test]
fn console_capture_reader_resumes_from_offsets_and_skips_partial_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("console.jsonl");
    std::fs::write(
        &path,
        concat!(
            r#"{"t":1,"s":"out","l":"2026-09-27T10:00:00Z  INFO app: hello"}"#,
            "\n",
            r#"{"t":2,"s":"err","l":"thread 'main' panicked at x"}"#,
            "\n",
            r#"{"t":3,"s":"out","l":"partial"#,
        ),
    )
    .unwrap();
    let (lines, next) = read_console_capture(&path, Some(0), 0).unwrap();
    assert_eq!(
        lines.len(),
        2,
        "the unterminated record is not returned yet"
    );
    assert_eq!(lines[0].level, Some(LogLevel::Info));
    assert_eq!(lines[1].stream, "err");
    assert_eq!(lines[1].level, Some(LogLevel::Error));
    // Finish the partial record; resuming from `next` returns just it.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    std::io::Write::write_all(&mut f, b"\"}\n").unwrap();
    let (more, _) = read_console_capture(&path, Some(next), 0).unwrap();
    assert_eq!(more.len(), 1);
    assert_eq!(more[0].text, "partial");
    assert_eq!(more[0].offset, next);
}

#[test]
fn level_parse_accepts_common_spellings() {
    assert_eq!(LogLevel::parse("WARNING"), Some(LogLevel::Warn));
    assert_eq!(LogLevel::parse(" error "), Some(LogLevel::Error));
    assert_eq!(LogLevel::parse("verbose"), None);
}

#[test]
fn victauri_and_rmcp_infrastructure_lines_are_not_captured() {
    let buf = Arc::new(LogBuffer::default());
    with_capture(&buf, || {
        tracing::info!(target: "rmcp::service", "Service initialized as server");
        tracing::warn!(target: "victauri_plugin", "VICTAURI INTROSPECTION SERVER ACTIVE");
        tracing::info!(target: "victauri_plugin::mcp::server", "listening");
        tracing::info!(target: "my_app::rmcp_client", "app code that merely mentions rmcp");
    });
    let page = buf.query(&LogQuery::default());
    assert_eq!(
        page.entries.len(),
        1,
        "only the app's own line: {:?}",
        page.entries
    );
    assert_eq!(page.entries[0].target, "my_app::rmcp_client");
}
#[test]
fn trim_backtrace_starts_at_the_code_that_panicked() {
    let raw = "   0: std::backtrace_rs::backtrace::win64::trace
             at /rustc/x/library/std/src/../../backtrace/src/backtrace/win64.rs:85
   1: std::backtrace::Backtrace::force_capture
             at /rustc/x/library/std/src/backtrace.rs:312
   2: victauri_plugin::backend_logs::capture_panic
             at ./crates/victauri-plugin/src/backend_logs.rs:1204
   3: alloc::boxed::impl$30::call
   4: std::panicking::panic_with_hook
   5: core::panicking::panic_bounds_check
   6: demo_app::panic_in_background::closure$0
             at ./examples/demo-app/src/main.rs:425
   7: std::sys::backtrace::__rust_begin_short_backtrace
   8: std::thread::lifecycle::spawn_unchecked";
    let t = trim_backtrace(raw);
    assert!(
        t.trim_start()
            .starts_with("6: demo_app::panic_in_background"),
        "machinery frames must be trimmed: {t}"
    );
    assert!(
        t.contains("main.rs:425"),
        "frame detail lines are kept: {t}"
    );
}

/// The `tauri-plugin-log` recipe: its `TargetKind::Dispatch` takes a `fern::Dispatch`
/// (re-exported as `tauri_plugin_log::fern`), and chaining Victauri's `log::Log` into it
/// must deliver every record that passes the plugin's level filter.
#[test]
fn log_logger_chains_into_a_fern_dispatch() {
    let buf = Arc::new(LogBuffer::default());
    let sink: Box<dyn log::Log> = Box::new(VictauriLogger::with_buffer(Arc::clone(&buf), None));
    let (_max, dispatch) = fern::Dispatch::new()
        .level(log::LevelFilter::Info)
        .chain(sink)
        .into_log();
    for (level, msg) in [
        (log::Level::Debug, "filtered out by the dispatch level"),
        (log::Level::Warn, "cache miss storm"),
    ] {
        dispatch.log(
            &log::Record::builder()
                .level(level)
                .target("tpl_app::cache")
                .args(format_args!("{msg}"))
                .build(),
        );
    }
    let page = buf.query(&LogQuery::default());
    assert_eq!(page.entries.len(), 1, "{:?}", page.entries);
    assert_eq!(page.entries[0].message, "cache miss storm");
    assert_eq!(page.entries[0].source, LogSource::Log);
}
/// Documented recipe: `registry().with(EnvFilter).with(fmt::layer()).with(log_layer())`.
/// Also pins the documented limit: the global filter applies to the capture layer too.
#[test]
fn registry_recipe_captures_what_the_global_filter_allows() {
    let buf = Arc::new(LogBuffer::default());
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("info"))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::sink))
        .with(Some(BackendLogLayer::with_buffer(Arc::clone(&buf))));
    tracing::subscriber::with_default(subscriber, || {
        tracing::debug!(target: "app", "below the app's filter");
        tracing::info!(target: "app", "kept");
    });
    let page = buf.query(&LogQuery::default());
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].message, "kept");
}

/// Documented recipe for `fmt().init()` apps: `fmt()...finish().with(log_layer())`.
#[test]
fn fmt_builder_recipe_captures() {
    let buf = Arc::new(LogBuffer::default());
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_writer(std::io::sink)
        .finish()
        .with(Some(BackendLogLayer::with_buffer(Arc::clone(&buf))));
    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("job", id = 3_u64);
        let _g = span.enter();
        tracing::warn!(target: "app", attempt = 2_u64, "retrying");
    });
    let e = &buf.query(&LogQuery::default()).entries[0];
    assert_eq!(e.fields["attempt"], 2);
    assert_eq!(
        e.spans,
        vec!["job{id=3}".to_string()],
        "span context works on fmt's subscriber too"
    );
}

/// `log_layer()` is `Option<Layer>`: `None` (release / `VICTAURI_DISABLE`) must be a valid,
/// inert layer so the app's line can stay unconditionally.
#[test]
fn a_none_layer_is_inert() {
    let subscriber = tracing_subscriber::registry().with(None::<BackendLogLayer>);
    tracing::subscriber::with_default(subscriber, || tracing::info!(target: "app", "fine"));
}
