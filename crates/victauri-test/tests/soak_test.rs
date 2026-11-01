//! Soak/longevity test: exercises Victauri continuously for several minutes
//! to detect memory leaks, `WeakRef` map growth, pending eval accumulation,
//! and response time degradation.
//!
//! Requires a running app with Victauri (e.g. `demo-app`):
//!   `cargo run -p demo-app`
//!
//! Then run with:
//!   `VICTAURI_SOAK=1 cargo test -p victauri-test --test soak_test -- --nocapture`
//!
//! Optional env vars:
//!   `VICTAURI_SOAK_DURATION_SECS`  — how long to run (default: 120)
//!   `VICTAURI_SOAK_PORT`           — override port (default: auto-discover)

use std::time::{Duration, Instant};
use victauri_test::VictauriClient;

fn skip_unless_soak() -> bool {
    std::env::var("VICTAURI_SOAK").is_ok()
}

fn soak_duration() -> Duration {
    let secs: u64 = std::env::var("VICTAURI_SOAK_DURATION_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);
    Duration::from_secs(secs)
}

#[tokio::test]
async fn soak_eval_and_snapshot() {
    if !skip_unless_soak() {
        eprintln!("skipping soak test (set VICTAURI_SOAK=1 to run)");
        return;
    }

    let mut client = VictauriClient::discover()
        .await
        .expect("failed to connect — is the app running?");

    let duration = soak_duration();
    let start = Instant::now();
    let mut iteration = 0u64;
    let mut eval_times = Vec::new();
    let mut snapshot_times = Vec::new();
    let mut find_times = Vec::new();
    let mut memory_samples = Vec::new();

    let initial_ws = client.memory_stats().await.unwrap().working_set_bytes;
    eprintln!(
        "soak test starting — duration: {}s, initial memory: {:.1} MB",
        duration.as_secs(),
        initial_ws as f64 / 1_048_576.0
    );

    while start.elapsed() < duration {
        iteration += 1;

        let t = Instant::now();
        let result = client.eval_js("document.title").await;
        let elapsed = t.elapsed();
        assert!(
            result.is_ok(),
            "eval_js failed at iteration {iteration}: {result:?}"
        );
        eval_times.push(elapsed);

        if iteration.is_multiple_of(10) {
            let t = Instant::now();
            let result = client.dom_snapshot().await;
            let elapsed = t.elapsed();
            assert!(
                result.is_ok(),
                "dom_snapshot failed at iteration {iteration}: {result:?}"
            );
            snapshot_times.push(elapsed);
        }

        if iteration.is_multiple_of(20) {
            let t = Instant::now();
            let result = client
                .find_elements(serde_json::json!({"selector": "button"}))
                .await;
            let elapsed = t.elapsed();
            assert!(
                result.is_ok(),
                "find_elements failed at iteration {iteration}: {result:?}"
            );
            find_times.push(elapsed);
        }

        if iteration.is_multiple_of(100)
            && let Ok(stats) = client.memory_stats().await
        {
            let ws = stats.working_set_bytes;
            memory_samples.push(ws);
            eprintln!(
                "  [{:.0}s] iteration {iteration}, memory: {:.1} MB, eval p50: {:.1}ms",
                start.elapsed().as_secs_f64(),
                ws as f64 / 1_048_576.0,
                percentile(&eval_times, 50) * 1000.0,
            );
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let final_ws = client.memory_stats().await.unwrap().working_set_bytes;

    eprintln!("\n=== SOAK TEST REPORT ===");
    eprintln!("Duration: {:.0}s", start.elapsed().as_secs_f64());
    eprintln!("Iterations: {iteration}");
    eprintln!(
        "Memory: {:.1} MB → {:.1} MB (delta: {:.1} MB)",
        initial_ws as f64 / 1_048_576.0,
        final_ws as f64 / 1_048_576.0,
        (final_ws as f64 - initial_ws as f64) / 1_048_576.0,
    );
    eprintln!(
        "eval_js:       p50={:.1}ms  p95={:.1}ms  p99={:.1}ms  ({} calls)",
        percentile(&eval_times, 50) * 1000.0,
        percentile(&eval_times, 95) * 1000.0,
        percentile(&eval_times, 99) * 1000.0,
        eval_times.len(),
    );
    eprintln!(
        "dom_snapshot:  p50={:.1}ms  p95={:.1}ms  p99={:.1}ms  ({} calls)",
        percentile(&snapshot_times, 50) * 1000.0,
        percentile(&snapshot_times, 95) * 1000.0,
        percentile(&snapshot_times, 99) * 1000.0,
        snapshot_times.len(),
    );
    eprintln!(
        "find_elements: p50={:.1}ms  p95={:.1}ms  p99={:.1}ms  ({} calls)",
        percentile(&find_times, 50) * 1000.0,
        percentile(&find_times, 95) * 1000.0,
        percentile(&find_times, 99) * 1000.0,
        find_times.len(),
    );

    let memory_growth_mb = (final_ws as f64 - initial_ws as f64) / 1_048_576.0;
    assert!(
        memory_growth_mb < 100.0,
        "memory grew by {memory_growth_mb:.1} MB — possible leak"
    );

    if eval_times.len() >= 40 {
        let quarter = eval_times.len() / 4;
        let first_quarter = &eval_times[..quarter];
        let last_quarter = &eval_times[eval_times.len() - quarter..];
        let first_p50 = percentile(first_quarter, 50);
        let last_p50 = percentile(last_quarter, 50);
        let degradation = last_p50 / first_p50.max(0.001);
        eprintln!(
            "Latency degradation: first-quarter p50={:.1}ms, last-quarter p50={:.1}ms, ratio={:.2}x",
            first_p50 * 1000.0,
            last_p50 * 1000.0,
            degradation,
        );
        assert!(
            degradation < 5.0,
            "eval_js latency degraded {degradation:.1}x over the soak period"
        );
    }
}

fn percentile(times: &[Duration], pct: usize) -> f64 {
    if times.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f64> = times.iter().map(Duration::as_secs_f64).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = (sorted.len() * pct / 100).min(sorted.len() - 1);
    sorted[idx]
}
