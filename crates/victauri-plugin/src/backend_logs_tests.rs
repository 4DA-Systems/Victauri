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
fn a_cursor_read_pages_forward_without_skipping_a_burst() {
    let buf = LogBuffer::default();
    for i in 0..10 {
        buf.push(entry(LogLevel::Info, "t", &format!("n{i}")));
    }
    // A follower 10 entries behind, reading 3 at a time, must see every entry exactly once.
    let mut cursor = 0;
    let mut seen = Vec::new();
    loop {
        let page = buf.query(&LogQuery {
            since_seq: Some(cursor),
            limit: 3,
            ..LogQuery::default()
        });
        seen.extend(page.entries.iter().map(|e| e.message.clone()));
        assert!(
            page.next_seq > cursor || page.entries.is_empty(),
            "cursor must advance"
        );
        cursor = page.next_seq;
        if !page.truncated {
            break;
        }
    }
    assert_eq!(seen, (0..10).map(|i| format!("n{i}")).collect::<Vec<_>>());
    assert_eq!(cursor, 10, "the last page hands back the end of the buffer");
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

/// `clear()` removes entries on purpose: a cursor from before the clear has missed nothing
/// to eviction, so neither a query nor a waiter may report one — but an eviction AFTER the
/// clear still is a gap.
#[tokio::test]
async fn a_clear_is_not_reported_as_an_eviction_gap() {
    let buf = LogBuffer::new(3, usize::MAX);
    for _ in 0..2 {
        buf.push(entry(LogLevel::Info, "t", "before"));
    }
    buf.clear();
    buf.push(entry(LogLevel::Info, "t", "after"));
    let page = buf.query(&LogQuery {
        since_seq: Some(0),
        ..LogQuery::default()
    });
    assert!(!page.gap, "cleared entries are not an eviction gap");
    assert_eq!(page.entries.len(), 1);
    let (hit, missed) = buf
        .wait_for(0, std::time::Duration::from_millis(20), |e| {
            e.message == "nope"
        })
        .await;
    assert!(hit.is_none());
    assert_eq!(missed, 0);

    // Overflow after the clear (capacity 3): seq 2 and 3 are evicted unseen.
    for _ in 0..4 {
        buf.push(entry(LogLevel::Info, "t", "later"));
    }
    let page = buf.query(&LogQuery {
        since_seq: Some(0),
        ..LogQuery::default()
    });
    assert!(page.gap);
    let (_, missed) = buf
        .wait_for(0, std::time::Duration::from_millis(20), |_| false)
        .await;
    assert_eq!(missed, 2);
}

/// `range` (what `invoke_command {with_logs}` attaches) and `wait_for` jump to the cursor by
/// index; both must still return exactly the entries in `[from, to)`.
#[tokio::test]
async fn cursor_lookups_return_exactly_the_requested_window() {
    let buf = LogBuffer::new(5, usize::MAX);
    for i in 0..8 {
        buf.push(entry(LogLevel::Info, "t", &format!("m{i}")));
    }
    // Entries 3..8 remain.
    let (got, total) = buf.range(4, 7, LogLevel::Trace, 100);
    let msgs: Vec<&str> = got.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(msgs, ["m4", "m5", "m6"]);
    assert_eq!(total, 3);
    let (got, _) = buf.range(0, 4, LogLevel::Trace, 100);
    assert_eq!(got.len(), 1, "only m3 is still buffered below 4");
    let (got, _) = buf.range(100, 200, LogLevel::Trace, 100);
    assert!(got.is_empty());
    let (hit, _) = buf
        .wait_for(6, std::time::Duration::from_millis(20), |e| {
            e.message.starts_with('m')
        })
        .await;
    assert_eq!(hit.map(|e| e.message), Some("m6".to_string()));
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
    let (hit, missed) = buf
        .wait_for(from, std::time::Duration::from_secs(5), |e| {
            e.message.contains("complete")
        })
        .await;
    assert_eq!(missed, 0);
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
    let (hit, _) = buf
        .wait_for(from, std::time::Duration::from_millis(100), |e| {
            e.message.contains("complete")
        })
        .await;
    assert!(
        hit.is_none(),
        "an entry before the cursor must not satisfy the wait"
    );
}

/// Entries evicted before a waiter could check them are reported, not silently skipped: the
/// line being waited for may be among them.
#[tokio::test]
async fn wait_for_reports_entries_evicted_before_it_could_check_them() {
    let buf = LogBuffer::new(2, DEFAULT_MAX_BYTES);
    for i in 0..5 {
        buf.push(entry(LogLevel::Info, "t", &format!("line {i}")));
    }
    let (hit, missed) = buf
        .wait_for(0, std::time::Duration::from_millis(50), |e| {
            e.message == "line 0"
        })
        .await;
    assert!(hit.is_none());
    assert_eq!(missed, 3, "seqs 0..3 were evicted before the scan");
}

/// The panic hook must record a panic even while another thread holds the buffer (an agent's
/// digest, another thread's log line) — and must not deadlock when the panicking thread holds
/// it itself.
#[test]
fn panic_push_waits_for_another_holder_but_never_self_deadlocks() {
    let buf = Arc::new(LogBuffer::default());
    let holder = Arc::clone(&buf);
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let t = std::thread::spawn(move || {
        let guard = holder.lock();
        held_tx.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(150));
        drop(guard);
    });
    held_rx.recv().unwrap();
    assert!(
        buf.try_push(entry(LogLevel::Error, "panic", "from another thread")),
        "a panic while ANOTHER thread holds the lock is recorded"
    );
    t.join().unwrap();
    assert_eq!(buf.next_seq(), 1);

    let guard = buf.lock();
    assert!(
        !buf.try_push(entry(LogLevel::Error, "panic", "same thread")),
        "a panic on the thread holding the lock is dropped, not deadlocked"
    );
    drop(guard);
    assert!(buf.try_push(entry(LogLevel::Error, "panic", "after release")));
}

/// tokio-console setups (`--cfg tokio_unstable` + Tokio's `tracing` feature) trace every waker
/// clone while Tokio holds a `Notify`'s waiter lock. Capturing that event and waking waiters
/// from it re-entered the lock and froze every logging thread of the app. In a normal build
/// Tokio emits nothing and this passes trivially. To exercise the real interleaving, build with
/// `RUSTFLAGS="--cfg tokio_unstable"` AND run with `VICTAURI_LOG_LEVEL=trace` (the waker events
/// are TRACE, below the default capture level) — and run this test alone, since the capture
/// level is read once per process. CI's "tokio-console configuration" step does exactly that;
/// with both defences removed it hangs at the 20 s bound there.
#[test]
fn waking_a_log_waiter_survives_tokio_tracing_its_own_wakers() {
    let buf = Arc::new(LogBuffer::default());
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let b = Arc::clone(&buf);
    std::thread::spawn(move || {
        with_capture(&b, || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let woken = rt.block_on(async {
                let waiter = {
                    let b = Arc::clone(&b);
                    tokio::spawn(async move {
                        b.wait_for(0, std::time::Duration::from_secs(5), |e| {
                            e.message == "done"
                        })
                        .await
                    })
                };
                for _ in 0..3 {
                    tokio::task::yield_now().await;
                }
                b.push(entry(LogLevel::Info, "t", "done"));
                waiter.await.unwrap().0.is_some()
            });
            let _ = done_tx.send(woken);
        });
    });
    assert_eq!(
        done_rx.recv_timeout(std::time::Duration::from_secs(20)),
        Ok(true),
        "the waiter must be woken — a hang here is the re-entrant notify deadlock"
    );
}

/// A panic's backtrace is captured on the panicking thread but symbolised only when read: the
/// placeholder is replaced by the resolved text and the id removed, wherever the entry sits.
#[test]
fn backtraces_are_symbolised_when_read_not_when_panicking() {
    let id = 4_000_000_u32; // far from the hook's real ids
    // A disabled backtrace: symbolising a real one loads this test binary's debug info under
    // std's global backtrace lock (seconds on Windows) and stalls every test that panics
    // meanwhile. The plumbing under test is the same.
    super::pending_backtraces().lock().unwrap().insert(
        id,
        Arc::new(super::PendingBacktrace {
            backtrace: std::backtrace::Backtrace::disabled(),
            resolved: OnceLock::new(),
        }),
    );
    let mut out = serde_json::json!({
        "entries": [{"source": "panic", "message": "boom", "fields": {"backtrace_id": id, "backtrace": "(symbolised when read)"}}],
        "other": {"source": "panic", "fields": {"backtrace_id": 999_999_999_u64}}
    });
    super::resolve_backtraces(&mut out);
    let fields = &out["entries"][0]["fields"];
    assert!(fields.get("backtrace_id").is_none(), "{out}");
    let bt = fields["backtrace"].as_str().unwrap();
    assert!(!bt.is_empty() && bt != "(symbolised when read)", "{bt}");
    // An unknown id is dropped, not left dangling.
    assert!(
        out["other"]["fields"].get("backtrace_id").is_none(),
        "{out}"
    );
}

/// Set in the child process of [`child_case`].
const CHILD_CASE: &str = "VICTAURI_TEST_CHILD_CASE";

/// Re-run the test `name` of this binary in its own process with `CHILD_CASE=case`. For state
/// that is process-wide once set: the global max level, `capture_level`.
fn child_case(name: &str, case: &str, envs: &[(&str, &str)]) {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("backend_logs::tests::{name}"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_CASE, case)
        .envs(envs.iter().copied())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "case {case} failed: {}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A layer with no level hint and no filter, like a profiler or telemetry layer.
struct CountAll(&'static std::sync::atomic::AtomicUsize);

impl<S: Subscriber> Layer<S> for CountAll {
    fn on_event(&self, _: &Event<'_>, _: Context<'_, S>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// Adding `log_layer()` must not take events away from the app's own layers: `Layered` takes
/// the max of the hints, and "no hint" (everything) loses to any `Some`.
#[test]
fn the_capture_layer_never_lowers_what_other_layers_receive() {
    use tracing::level_filters::LevelFilter;
    use tracing_subscriber::util::SubscriberInitExt;

    static SEEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let Ok(case) = std::env::var(CHILD_CASE) else {
        let name = "the_capture_layer_never_lowers_what_other_layers_receive";
        child_case(name, "plain_outer", &[]);
        child_case(name, "plain_outer", &[("VICTAURI_LOG_LEVEL", "error")]);
        child_case(name, "plain_inner", &[]);
        child_case(name, "per_layer_filter", &[]);
        return;
    };
    let capture = BackendLogLayer::with_buffer(Arc::new(LogBuffer::default()));
    match case.as_str() {
        "plain_outer" => tracing_subscriber::registry()
            .with(CountAll(&SEEN))
            .with(capture)
            .init(),
        "plain_inner" => tracing_subscriber::registry()
            .with(capture)
            .with(CountAll(&SEEN))
            .init(),
        // Every other layer is filtered per layer: the case the hint exists for. Trace-level
        // callsites stay off, so dependencies do not build events for this layer alone.
        "per_layer_filter" => {
            tracing_subscriber::registry()
                .with(CountAll(&SEEN).with_filter(LevelFilter::INFO))
                .with(capture)
                .init();
            assert_eq!(LevelFilter::current(), LevelFilter::DEBUG);
            return;
        }
        other => panic!("unknown case {other}"),
    }
    assert_eq!(LevelFilter::current(), LevelFilter::TRACE);
    tracing::trace!("a");
    tracing::debug!("b");
    tracing::info!("c");
    assert_eq!(SEEN.load(Ordering::Relaxed), 3);
}

/// Writes through `f` and unwraps every write: harmless against a `String`, as app code expects.
struct UnwrapsWrites(usize);

impl UnwrapsWrites {
    fn write(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for _ in 0..self.0 / 100 {
            f.write_str(&"x".repeat(100)).unwrap();
        }
        Ok(())
    }
}

impl std::fmt::Debug for UnwrapsWrites {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.write(f)
    }
}

impl std::fmt::Display for UnwrapsWrites {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.write(f)
    }
}

/// A `Display` that fails, as a broken app type might.
#[derive(Debug)]
struct DisplayFails;

impl std::fmt::Display for DisplayFails {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Err(std::fmt::Error)
    }
}

impl std::error::Error for DisplayFails {}

/// Past a capture budget the bounded writer must not hand an app's `Debug`/`Display` an error it
/// never sees against a `String`: an impl that unwraps its writes would panic in the app's log
/// call, on whatever thread made it.
#[test]
#[allow(clippy::items_after_statements)]
fn a_log_call_does_not_panic_in_the_capture_path() {
    let buf = Arc::new(LogBuffer::default());
    with_capture(&buf, || {
        tracing::info!(target: "app", value = ?UnwrapsWrites(MAX_FIELD_BYTES + 1000), "over the field budget");
        tracing::info!(target: "app", "{}", UnwrapsWrites(MAX_MESSAGE_BYTES + 1000));
        let err: &(dyn std::error::Error + 'static) = &DisplayFails;
        tracing::info!(target: "app", cause = err, "an error field whose Display fails");
    });
    let page = buf.query(&LogQuery::default());
    assert_eq!(page.entries.len(), 3);
    let cut = page.entries[0].fields["value"].as_str().unwrap();
    assert!(cut.ends_with("…[truncated]"), "{cut}");
    assert!(page.entries[1].message.contains("…["));

    // The `log` path, including a record the wrapped logger would have dropped.
    struct DropsAll;
    impl log::Log for DropsAll {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            false
        }
        fn log(&self, _: &log::Record<'_>) {}
        fn flush(&self) {}
    }
    let logger = VictauriLogger::with_buffer(Arc::clone(&buf), Some(Box::new(DropsAll)));
    let send = |args: std::fmt::Arguments<'_>| {
        log::Log::log(
            &logger,
            &log::Record::builder()
                .args(args)
                .level(log::Level::Info)
                .target("app")
                .build(),
        );
    };
    send(format_args!("{}", UnwrapsWrites(MAX_MESSAGE_BYTES + 1000)));
    send(format_args!("{DisplayFails}"));
    assert_eq!(buf.query(&LogQuery::default()).entries.len(), 5);
}

/// A value far beyond the budget still stops early, so a multi-megabyte `Debug` on every event
/// costs a bounded amount and not its whole size.
#[test]
fn a_huge_debug_value_stops_being_formatted() {
    struct Counts<'a>(&'a std::cell::Cell<usize>);
    impl std::fmt::Debug for Counts<'_> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            for _ in 0..1_000_000 {
                self.0.set(self.0.get() + 1);
                f.write_str(&"x".repeat(100))?;
            }
            Ok(())
        }
    }
    let writes = std::cell::Cell::new(0);
    let text = super::debug_bounded(&Counts(&writes), MAX_FIELD_BYTES);
    assert!(text.ends_with("…[truncated]"));
    assert!(writes.get() < 10_000, "{} writes", writes.get());
}

/// A recorded value whose `Debug` logs inside the span used to deadlock against the span's
/// extensions lock, which `on_record` held across formatting.
#[test]
fn recording_a_value_that_logs_does_not_deadlock() {
    struct LogsWhileFormatting;
    impl std::fmt::Debug for LogsWhileFormatting {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            tracing::info!(target: "app", "logged while formatting");
            f.write_str("value")
        }
    }
    let (done, finished) = std::sync::mpsc::channel();
    let buf = Arc::new(LogBuffer::default());
    let in_thread = Arc::clone(&buf);
    std::thread::spawn(move || {
        with_capture(&in_thread, || {
            let span = tracing::info_span!("s", v = tracing::field::Empty);
            let _in_span = span.enter();
            span.record("v", tracing::field::debug(LogsWhileFormatting));
            tracing::info!(target: "app", "after");
        });
        let _ = done.send(());
    });
    finished
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("the thread deadlocked inside on_record");
    let page = buf.query(&LogQuery {
        contains: Some("after".into()),
        ..LogQuery::default()
    });
    assert_eq!(page.entries[0].spans, ["s{v=value}"]);
}

/// A duplicated `log_layer()` line: a harmless duplicate entry, never a panic in every new span.
#[test]
fn two_capture_layers_in_one_subscriber_do_not_panic() {
    let buf = Arc::new(LogBuffer::default());
    let subscriber = tracing_subscriber::registry()
        .with(BackendLogLayer::with_buffer(Arc::clone(&buf)))
        .with(BackendLogLayer::with_buffer(Arc::clone(&buf)));
    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("s", v = 1_u64);
        let _in_span = span.enter();
        span.record("v", 2_u64);
        tracing::info!(target: "app", "inside");
    });
    assert_eq!(buf.query(&LogQuery::default()).entries.len(), 2);
}

/// `backtrace_id` is only meaningful in a captured panic: an app's own field of that name, and
/// anything else that merely looks like one, is data and comes back untouched.
#[test]
fn resolving_backtraces_leaves_app_data_alone() {
    let id = 4_000_000_001_u32;
    super::pending_backtraces().lock().unwrap().insert(
        id,
        Arc::new(super::PendingBacktrace {
            backtrace: std::backtrace::Backtrace::disabled(),
            resolved: OnceLock::new(),
        }),
    );
    let mut out = serde_json::json!({
        "result": {"backtrace_id": id, "user": "kept"},
        "backend_logs": [
            {"source": "tracing", "message": "hi", "fields": {"backtrace_id": id, "job": 1}},
            {"source": "panic", "message": "boom", "fields": {"backtrace_id": id}},
        ],
    });
    super::resolve_backtraces(&mut out);
    assert_eq!(out["result"]["backtrace_id"], id, "{out}");
    let logs = &out["backend_logs"];
    assert_eq!(logs[0]["fields"]["backtrace_id"], id, "{out}");
    assert!(logs[1]["fields"].get("backtrace_id").is_none(), "{out}");
    assert!(logs[1]["fields"]["backtrace"].is_string(), "{out}");
}

/// Logging threads (the UI thread among them) need the ring lock on every event, so a read must
/// copy under it and work after releasing it. Measured at the maximum capacity, where `digest`
/// held the lock for a second.
#[test]
fn reading_the_log_does_not_hold_up_logging_threads() {
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    let buf = Arc::new(LogBuffer::new(100_000, usize::MAX));
    let message = "m".repeat(500);
    for i in 0..100_000 {
        buf.push(entry(LogLevel::Warn, "t", &format!("{i} {message}")));
    }
    let stop = Arc::new(AtomicBool::new(false));
    let logger = {
        let (buf, stop) = (Arc::clone(&buf), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut worst = Duration::ZERO;
            while !stop.load(Ordering::Relaxed) {
                let line = entry(LogLevel::Info, "t", "x");
                let started = Instant::now();
                buf.push(line);
                worst = worst.max(started.elapsed());
                std::thread::yield_now();
            }
            worst
        })
    };
    std::thread::sleep(Duration::from_millis(20));
    let started = Instant::now();
    let _digest = buf.digest(10, 10);
    let _matches = buf.query(&LogQuery {
        contains: Some("nothing has this".into()),
        ..LogQuery::default()
    });
    let reading = started.elapsed();
    stop.store(true, Ordering::Relaxed);
    let worst = logger.join().unwrap();
    assert!(
        worst < reading / 2,
        "a log call stalled {worst:?} during reads taking {reading:?}"
    );
}

/// A wait's predicate runs off the lock too: it may redact, and it must not stall the app.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wait_predicate_does_not_run_under_the_lock() {
    let buf = Arc::new(LogBuffer::default());
    buf.push(entry(LogLevel::Info, "t", "first"));
    let pushed_inside = Arc::clone(&buf);
    let (hit, _) = buf
        .wait_for(0, std::time::Duration::from_secs(20), move |e| {
            let other = Arc::clone(&pushed_inside);
            let (pushed, done) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                other.push(entry(LogLevel::Info, "t", "second"));
                let _ = pushed.send(());
            });
            done.recv_timeout(std::time::Duration::from_secs(5))
                .expect("a logging thread stalled behind the predicate");
            e.message == "first"
        })
        .await;
    assert_eq!(hit.unwrap().message, "first");
}

/// A huge `Debug` value costs at most the field budget, not its whole size.
#[test]
fn debug_values_are_bounded_while_formatting() {
    let big = vec![0_u8; 1_000_000];
    let text = super::debug_bounded(&big, MAX_FIELD_BYTES);
    assert!(
        text.len() <= MAX_FIELD_BYTES + "…[truncated]".len(),
        "{}",
        text.len()
    );
    assert!(text.ends_with("…[truncated]"));
    assert_eq!(super::debug_bounded(&"short", MAX_FIELD_BYTES), "\"short\"");
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

/// Review of the CLI follower's rotation gap: a cursor is a byte offset, and once `victauri run`
/// rotated the capture it pointed into the middle of a different file. The unread tail of the
/// rotated file was lost and the new file's start skipped, with nothing said.
#[test]
fn a_cursor_read_survives_a_rotation() {
    fn record(n: usize, text: &str) -> String {
        format!("{{\"t\":{n},\"s\":\"out\",\"l\":\"{text}{n}\"}}\n")
    }
    /// All a follower reads from `cursor` on, as it would: on while `more`, then one more poll.
    fn follow(path: &std::path::Path, mut cursor: u64) -> Vec<String> {
        let mut out = Vec::new();
        for _ in 0..8 {
            let (lines, next, _) =
                read_console_capture_window(path, Some(cursor), 0, CONSOLE_READ_WINDOW).unwrap();
            out.extend(lines.into_iter().map(|l| l.text));
            cursor = next;
        }
        out
    }
    for new_file_lines in [1usize, 50] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.jsonl");
        let old: String = (0..4).map(|n| record(n, "old")).collect();
        std::fs::write(&path, &old).unwrap();
        let (_, cursor) = read_console_capture(&path, Some(0), 0).unwrap();
        let cursor = cursor - (2 * record(0, "old").len()) as u64;

        std::fs::rename(&path, path.with_extension("jsonl.1")).unwrap();
        // Short: the cursor is past its end. Long: the cursor lands inside a record.
        let new: String = (0..new_file_lines)
            .map(|n| record(n, "a-new-line-of-another-length-"))
            .collect();
        std::fs::write(&path, &new).unwrap();

        let got = follow(&path, cursor);
        let mut want = vec!["old2".to_string(), "old3".to_string()];
        want.extend((0..new_file_lines).map(|n| format!("a-new-line-of-another-length-{n}")));
        assert_eq!(got, want, "new file of {new_file_lines} line(s)");
    }
}

/// A `victauri run` that was killed writes no exit record. The app can outlive it (SIGKILL on
/// Unix), and then `logs stdout` read on as if all were well, with nothing more ever to come.
#[test]
fn a_capture_says_when_its_launcher_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("4242-1730000000000.jsonl");
    std::fs::write(&path, capture_record(0, "line")).unwrap();
    assert_eq!(
        launcher_gone(&path, Some(4242), |pid| pid == 4242),
        Some(4242)
    );
    assert_eq!(
        launcher_gone(&path, Some(4242), |_| false),
        None,
        "the launcher is alive"
    );

    let exit = "{\"t\":9,\"s\":\"exit\",\"l\":\"exited with code 3\",\"tail\":[]}\n";
    std::fs::write(&path, format!("{}{exit}", capture_record(0, "line"))).unwrap();
    assert_eq!(
        launcher_gone(&path, Some(4242), |_| true),
        None,
        "it said how it ended"
    );

    // A `--capture` file's launcher does not say: a name that looks like a pid is not one.
    let named = dir.path().join("20261102-120000.jsonl");
    std::fs::write(&named, capture_record(0, "line")).unwrap();
    assert_eq!(launcher_gone(&named, None, |_| true), None);
}

/// An exit record carries up to 12 stderr lines of 64 KiB: the tail read must hold all of it.
#[test]
fn a_long_exit_record_is_seen_as_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("4242-1730000000000.jsonl");
    let tail: Vec<String> = (0..12).map(|_| "x".repeat(64 * 1024)).collect();
    let exit = serde_json::json!({"t": 9, "s": "exit", "l": "crashed", "tail": tail});
    std::fs::write(&path, format!("{}{exit}\n", capture_record(0, "line"))).unwrap();
    assert_eq!(launcher_gone(&path, Some(4242), |_| true), None);
}

/// On Linux, a real process: a zombie its parent has not reaped is gone too.
#[cfg(target_os = "linux")]
#[test]
fn a_dead_process_is_seen_as_dead() {
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    assert!(!process_is_dead(child.id()));
    assert!(!process_is_dead_by_shell(child.id()));
    child.kill().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(process_is_dead(child.id()), "unreaped");
    assert!(process_is_dead_by_shell(child.id()), "unreaped, by ps");
    child.wait().unwrap();
    assert!(process_is_dead(child.id()), "reaped");
    assert!(process_is_dead_by_shell(child.id()), "reaped, by kill");
    assert!(!process_is_dead(std::process::id()));
    assert!(!process_is_dead_by_shell(std::process::id()));
}

/// A bare offset cannot see a rotation whose new file has a record boundary exactly there: the
/// rotated file's tail and the new file's start went unread, with nothing said.
#[test]
fn a_cursor_on_a_record_boundary_of_the_new_file_loses_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.jsonl");
    let record = capture_record;
    let old: String = (0..4).map(|n| record(n, "old")).collect();
    std::fs::write(&path, &old).unwrap();
    // The first two read.
    let cursor = (2 * record(0, "old").len()) as u64;
    let read = read_console_cursor(&path, Some(0), 0, cursor).unwrap();
    assert_eq!(read.lines.len(), 2);
    let cursor = read.cursor(read.next);

    std::fs::rename(&path, path.with_extension("jsonl.1")).unwrap();
    let new: String = (0..4).map(|n| record(n, "new")).collect();
    std::fs::write(&path, &new).unwrap();

    let (got, at, lost) = follow_cursor(&path, cursor);
    assert_eq!(got, ["old2", "old3", "new0", "new1", "new2", "new3"]);
    assert!(!lost);
    // And on from there: the new file grew, nothing again from before.
    let mut more = new.clone();
    more.push_str(&record(4, "new"));
    std::fs::write(&path, &more).unwrap();
    assert_eq!(follow_cursor(&path, at).0, ["new4"]);
}

/// All a follower reads from `cursor` on, as it would, with where it ended and whether it was told
/// that lines were lost.
fn follow_cursor(path: &std::path::Path, mut cursor: u64) -> (Vec<String>, u64, bool) {
    let (mut got, mut lost) = (Vec::new(), false);
    for _ in 0..8 {
        let read = read_console_cursor(path, Some(cursor), 0, CONSOLE_READ_WINDOW).unwrap();
        lost |= read.lost;
        cursor = read.cursor(read.next);
        got.extend(read.lines.into_iter().map(|l| l.text));
    }
    (got, cursor, lost)
}

fn capture_record(n: usize, text: &str) -> String {
    format!("{{\"t\":{n},\"s\":\"out\",\"l\":\"{text}{n}\"}}\n")
}

/// Two rotations between reads: the file the cursor was in is gone, and that is said.
#[test]
fn a_cursor_whose_file_rotated_away_says_lines_were_lost() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.jsonl");
    std::fs::write(&path, capture_record(0, "first")).unwrap();
    let read = read_console_cursor(&path, Some(0), 0, CONSOLE_READ_WINDOW).unwrap();
    let cursor = read.cursor(read.next);

    std::fs::rename(&path, path.with_extension("jsonl.1")).unwrap();
    std::fs::write(&path, capture_record(0, "second")).unwrap();
    std::fs::remove_file(path.with_extension("jsonl.1")).unwrap();
    std::fs::rename(&path, path.with_extension("jsonl.1")).unwrap();
    std::fs::write(&path, capture_record(0, "third")).unwrap();

    let first_read = read_console_cursor(&path, Some(cursor), 0, CONSOLE_READ_WINDOW).unwrap();
    assert!(first_read.lost);
    let at = first_read.cursor(first_read.next);
    assert_eq!(first_read.lines[0].text, "third0");
    let (got, _, lost) = follow_cursor(&path, at);
    assert!(got.is_empty() && !lost, "said once: {got:?}");
}

/// A cursor from before tags, or a line's `offset`, reads as it always did.
#[test]
fn a_bare_offset_is_still_a_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.jsonl");
    let body: String = (0..3).map(|n| capture_record(n, "line")).collect();
    std::fs::write(&path, &body).unwrap();
    let offset = capture_record(0, "line").len() as u64;
    let (got, at, lost) = follow_cursor(&path, offset);
    assert_eq!(got, ["line1", "line2"]);
    assert!(!lost);
    assert!(
        at > body.len() as u64,
        "what it returns carries the tag: {at}"
    );
    let line_offset = read_console_cursor(&path, Some(at), 0, CONSOLE_READ_WINDOW)
        .unwrap()
        .next;
    assert_eq!(line_offset, body.len() as u64);

    // Past a rotation, by where it lands.
    std::fs::rename(&path, path.with_extension("jsonl.1")).unwrap();
    std::fs::write(
        &path,
        capture_record(0, "new-and-longer-than-the-old-file-was"),
    )
    .unwrap();
    let (got, _, lost) = follow_cursor(&path, offset);
    assert_eq!(
        got,
        ["line1", "line2", "new-and-longer-than-the-old-file-was0"]
    );
    assert!(!lost);
}

/// JavaScript clients read a cursor as a double, and a file with no complete record has no tag.
#[test]
fn a_cursor_is_exact_in_a_double_and_needs_a_first_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.jsonl");
    std::fs::write(&path, "{\"t\":1,\"s\":\"out\",\"l\":\"partial").unwrap();
    assert_eq!(capture_tag(&path), None);
    let read = read_console_cursor(&path, Some(0), 0, CONSOLE_READ_WINDOW).unwrap();
    assert_eq!(read.cursor(read.next), 0);

    std::fs::write(&path, capture_record(0, "line")).unwrap();
    let read = read_console_cursor(&path, Some(0), 0, CONSOLE_READ_WINDOW).unwrap();
    assert!(capture_tag(&path).is_some_and(|tag| tag > 0 && tag < 1 << CURSOR_TAG_BITS));
    let cursor = read.cursor((1 << CURSOR_OFFSET_BITS) - 1);
    assert!(cursor < 1 << 53);
    assert_eq!(
        cursor & ((1 << CURSOR_OFFSET_BITS) - 1),
        (1 << CURSOR_OFFSET_BITS) - 1
    );
    assert_eq!(
        read.cursor(1 << CURSOR_OFFSET_BITS),
        1 << CURSOR_OFFSET_BITS,
        "no room: bare"
    );
}

/// Release review: a cursor read stops at its byte window (16 MiB), and `logs stdout` reported
/// `truncated: false` whenever the LINE limit was not hit — on a 40 MB capture an agent read
/// the first window, saw nothing truncated, and never reached the panic further on. The reader
/// now says when it stopped at the window with more of the file after it.
#[test]
fn a_cursor_read_cut_by_its_window_says_there_is_more() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("console.jsonl");
    let record = |i: usize| format!("{{\"t\":{i},\"s\":\"out\",\"l\":\"line {i:03}\"}}\n");
    let body: String = (0..10).map(record).collect();
    std::fs::write(&path, &body).unwrap();
    // A window of about three records: page through the file by the returned cursor.
    let window = (record(0).len() * 3 + 5) as u64;
    let (mut cursor, mut seen, mut reads) = (0_u64, Vec::new(), 0);
    loop {
        let (lines, next, more) =
            read_console_capture_window(&path, Some(cursor), 0, window).unwrap();
        reads += 1;
        assert!(
            next > cursor || !more,
            "a read that says `more` must advance"
        );
        seen.extend(lines.into_iter().map(|l| l.text));
        cursor = next;
        if !more {
            break;
        }
    }
    assert_eq!(
        seen,
        (0..10).map(|i| format!("line {i:03}")).collect::<Vec<_>>()
    );
    assert!(
        reads >= 4,
        "the window cut the file into pages ({reads} reads)"
    );

    // The end of the file is not "more", even with a record still being written there.
    std::fs::write(
        &path,
        format!("{body}{{\"t\":99,\"s\":\"out\",\"l\":\"partial"),
    )
    .unwrap();
    let (_, next, more) =
        read_console_capture_window(&path, Some(0), 0, CONSOLE_READ_WINDOW).unwrap();
    assert!(!more, "an unterminated last record is not more to read");
    assert_eq!(next, body.len() as u64);

    // A window smaller than one record consumes nothing: it must not ask for a re-read of
    // the same offset (a follower would spin on it).
    let (lines, next, more) = read_console_capture_window(&path, Some(0), 0, 4).unwrap();
    assert!(lines.is_empty());
    assert_eq!(next, 0);
    assert!(!more);
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

/// Documented limit: the capture leaves the embedded MCP SDK's lines out, the app's console does
/// not, and the filter the page gives hides them (`rmcp::service` logs at INFO during MCP calls).
#[test]
fn the_documented_filter_hides_the_sdks_lines_from_the_console() {
    #[derive(Clone, Default)]
    struct Console(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Console {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let run = |filter: &str| {
        let console = Console::default();
        let writer = console.clone();
        let buf = Arc::new(LogBuffer::default());
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new(filter))
            .with(tracing_subscriber::fmt::layer().with_writer(move || writer.clone()))
            .with(Some(BackendLogLayer::with_buffer(Arc::clone(&buf))));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "rmcp::service", "Service initialized as server");
            tracing::info!(target: "app", "kept");
        });
        let page = buf.query(&LogQuery::default());
        assert_eq!(page.entries.len(), 1, "{:?}", page.entries);
        assert_eq!(page.entries[0].message, "kept");
        let printed = console.0.lock().unwrap().clone();
        String::from_utf8_lossy(&printed).into_owned()
    };
    let plain = run("info");
    assert!(plain.contains("Service initialized"), "{plain}");
    assert!(plain.contains("kept"), "{plain}");
    let quiet = run("info,rmcp=warn");
    assert!(!quiet.contains("Service initialized"), "{quiet}");
    assert!(quiet.contains("kept"), "{quiet}");
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

/// A reader racing a writer that rotates as `victauri run` does: every line arrives once or is
/// reported lost. The tag and the bytes used to come from two opens, so a rotation between them read
/// the new file from an offset meant for the old one, and the lines before it went unsaid.
#[test]
fn a_reader_racing_rotations_loses_no_line_silently() {
    use std::io::Write as _;
    const TOTAL: u64 = 150_000;
    const ROTATE_AT: u64 = 80_000;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.jsonl");
    let write_line = |file: &mut std::fs::File, i: u64| {
        let line = format!(
            "{{\"t\":{i},\"s\":\"out\",\"l\":\"{i} {}\"}}\n",
            "x".repeat(60)
        );
        file.write_all(line.as_bytes()).unwrap();
        line.len() as u64
    };
    let mut first = std::fs::File::create(&path).unwrap();
    let mut written = write_line(&mut first, 0);
    let writer = {
        let path = path.clone();
        std::thread::spawn(move || {
            let mut file = first;
            let mut rotations = 0u32;
            for i in 1..TOTAL {
                written += write_line(&mut file, i);
                if written >= ROTATE_AT {
                    drop(file);
                    // A reader may hold either file open: renaming has to work regardless.
                    std::fs::rename(&path, path.with_extension("jsonl.1")).unwrap();
                    file = std::fs::File::create(&path).unwrap();
                    written = 0;
                    rotations += 1;
                }
                if i % 200 == 0 {
                    std::thread::sleep(std::time::Duration::from_micros(200));
                }
            }
            rotations
        })
    };
    let mut cursor = 0;
    let mut expected = 0u64;
    let (mut silent, mut reported, mut duplicates) = (0u64, 0u64, 0u64);
    let give_up = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while expected < TOTAL && std::time::Instant::now() < give_up {
        // The instant between the rename and the new file has no file: the tool says so, and the
        // next call reads on.
        let Ok(read) = read_console_cursor(&path, Some(cursor), 1 << 20, 1 << 24) else {
            continue;
        };
        for line in &read.lines {
            let i: u64 = line.text.split(' ').next().unwrap().parse().unwrap();
            if i < expected {
                duplicates += 1;
                continue;
            }
            if i > expected {
                if read.lost {
                    reported += i - expected;
                } else {
                    silent += i - expected;
                }
            }
            expected = i + 1;
        }
        cursor = read.cursor(read.next);
    }
    let rotations = writer.join().unwrap();
    assert!(rotations > 100, "{rotations} rotations");
    assert_eq!(
        (silent, duplicates),
        (0, 0),
        "silently lost, duplicated; reported lost {reported}, over {rotations} rotations"
    );
    assert_eq!(expected, TOTAL, "reported lost {reported}");
}
