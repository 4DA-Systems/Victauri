//! Round-5 (tools lane) regressions for the injected JS bridge (jsdom via Node.js).
//!
//! Same harness as `bridge_tests.rs` (`tests/bridge_tests/run_tests.js`). Set
//! `VICTAURI_REQUIRE_JSDOM=1` (CI sets `CI`) so a missing jsdom fails instead of skipping.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use victauri_plugin::js_bridge::{BridgeCapacities, init_script};

#[derive(Serialize)]
struct TestDef {
    bridge_script: String,
    setup_html: String,
    tests: Vec<TestCase>,
}

#[derive(Serialize)]
struct TestCase {
    name: String,
    code: String,
}

#[derive(Deserialize, Debug)]
struct TestResult {
    name: String,
    passed: bool,
    result: Option<serde_json::Value>,
    error: Option<String>,
}

fn runner_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("bridge_tests")
}

fn run_tests(tests: Vec<TestCase>) -> Option<Vec<TestResult>> {
    if !runner_dir().join("node_modules").join("jsdom").exists() {
        assert!(
            std::env::var_os("CI").is_none()
                && std::env::var_os("VICTAURI_REQUIRE_JSDOM").is_none(),
            "jsdom is not installed, so the JS bridge tests cannot run: \
             `npm ci` in crates/victauri-plugin/tests/bridge_tests/"
        );
        eprintln!("SKIP: jsdom not installed (run `npm ci` in tests/bridge_tests/)");
        return None;
    }
    let def = TestDef {
        bridge_script: init_script(&BridgeCapacities::default()),
        setup_html: r#"<html lang="en"><head><title>R5B</title></head><body><div id="app"></div></body></html>"#
            .to_string(),
        tests,
    };
    let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
    tmp.write_all(serde_json::to_string(&def).unwrap().as_bytes())
        .expect("write test def");
    tmp.flush().expect("flush temp file");
    let output = Command::new("node")
        .arg(runner_dir().join("run_tests.js"))
        .arg(tmp.path())
        .output()
        .expect("failed to run node");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stdout
        .lines()
        .find(|l| l.starts_with("VICTAURI_RESULTS:"))
        .unwrap_or_else(|| panic!("No VICTAURI_RESULTS line.\nstdout: {stdout}\nstderr: {stderr}"));
    let results: Vec<TestResult> = serde_json::from_str(&line["VICTAURI_RESULTS:".len()..])
        .unwrap_or_else(|e| panic!("bad results JSON: {e}\nraw: {line}"));
    let failures: Vec<String> = results
        .iter()
        .filter(|r| !r.passed)
        .map(|r| format!("  FAIL: {} => {:?}", r.name, r.error))
        .collect();
    assert!(
        failures.is_empty(),
        "Test failures:\n{}",
        failures.join("\n")
    );
    Some(results)
}

fn case(name: &str, code: &str) -> TestCase {
    TestCase {
        name: name.into(),
        code: code.into(),
    }
}

/// R5B-WAIT0: `timeout_ms: 0` used to be read as the 10 s default by the page (the server
/// waited only 5 s for it, so the call failed as a misleading "eval timed out"). 0 now means a
/// single immediate check: met → ok, not met → a prompt `ok:false`.
#[test]
fn wait_for_with_a_zero_timeout_checks_once() {
    let Some(results) = run_tests(vec![case(
        "waitFor timeout_ms 0",
        r"
        var V = window.__VICTAURI__;
        var t0 = Date.now();
        var met = await V.waitFor({ condition: 'selector', value: '#app', timeout_ms: 0, poll_ms: 200 });
        var unmet = await V.waitFor({ condition: 'selector', value: '#nope', timeout_ms: 0, poll_ms: 200 });
        // The condition is also checked once more AT the deadline, not skipped by it.
        setTimeout(function() {
            var d = document.createElement('p'); d.id = 'late'; document.body.appendChild(d);
        }, 60);
        var late = await V.waitFor({ condition: 'selector', value: '#late', timeout_ms: 100, poll_ms: 5000 });
        return { met: met, unmet: unmet, late: late, took: Date.now() - t0 };
        ",
    )]) else {
        return;
    };
    let r = results[0].result.clone().unwrap_or_default();
    assert_eq!(r["met"]["ok"], true, "{r}");
    assert_eq!(r["unmet"]["ok"], false, "{r}");
    assert!(r["took"].as_u64().unwrap() < 2_000, "{r}");
    assert_eq!(r["late"]["ok"], true, "{r}");
}
