//! Round-4 audit regressions for the injected JS bridge (jsdom via Node.js).
//!
//! Same harness as `bridge_tests.rs` (`tests/bridge_tests/run_tests.js`): each test serializes
//! a batch of JS cases, runs them in a fresh jsdom with the real bridge injected, and asserts
//! on the results. Set `VICTAURI_REQUIRE_JSDOM=1` (CI sets `CI`) so a missing jsdom fails
//! instead of skipping.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use victauri_plugin::js_bridge::{BridgeCapacities, init_script};

#[derive(Serialize)]
struct TestDef {
    bridge_script: String,
    setup_html: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    setup_js: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    node_web_api: bool,
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

fn run_tests(def: &TestDef) -> Option<Vec<TestResult>> {
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
    let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
    let json = serde_json::to_string(def).expect("serialize test def");
    tmp.write_all(json.as_bytes()).expect("write test def");
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
    Some(
        serde_json::from_str(&line["VICTAURI_RESULTS:".len()..])
            .unwrap_or_else(|e| panic!("bad results JSON: {e}\nraw: {line}")),
    )
}

fn assert_all_pass(results: &[TestResult]) {
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
}

fn case(name: &str, code: &str) -> TestCase {
    TestCase {
        name: name.into(),
        code: code.into(),
    }
}

fn def(setup_js: Option<String>, tests: Vec<TestCase>) -> TestDef {
    TestDef {
        bridge_script: init_script(&BridgeCapacities::default()),
        setup_html: r#"<html lang="en"><head><title>R4</title></head><body><div id="app"></div></body></html>"#
            .to_string(),
        setup_js,
        node_web_api: false,
        tests,
    }
}

fn result(results: &[TestResult], i: usize) -> serde_json::Value {
    results[i].result.clone().unwrap_or_default()
}

// ── R4-JS1: page script cannot forge a pagehide / pageshow ───────────────────

/// A synthetic (`isTrusted === false`) `pagehide` used to wipe every captured log and
/// permanently unhook console + DOM-mutation capture. The bridge now reacts only to the
/// browser's own (trusted) page-transition events.
#[test]
fn r4_js1_synthetic_pagehide_cannot_wipe_logs_or_unhook_capture() {
    let def = def(
        None,
        vec![
            case(
                "untrusted pagehide / pageshow are ignored",
                r"
                var V = window.__VICTAURI__;
                console.log('before');
                await fetch('http://ipc.localhost/some_cmd', { method: 'POST', body: '{}' });
                var hooked = console.log;
                window.dispatchEvent(new PageTransitionEvent('pagehide', { persisted: false }));
                window.dispatchEvent(new Event('pagehide'));
                window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
                console.log('after');
                document.getElementById('app').appendChild(document.createElement('p'));
                await new Promise(function(r) { setTimeout(r, 250); });
                return {
                    console: V.getConsoleLogs().map(function(l) { return l.message; }),
                    network: V.getNetworkLog().length,
                    ipc: V.getIpcLog().length,
                    still_hooked: console.log === hooked,
                    mutations: V.getMutationLog().length,
                };
                ",
            ),
            // Control: the browser's real (trusted) events keep their documented behaviour.
            case(
                "trusted pagehide still tears down, trusted persisted pageshow restores",
                r"
                var V = window.__VICTAURI__;
                console.log('before');
                window.__vtestTrustedPageTransition('pagehide', false);
                var cleared = V.getConsoleLogs().length;
                window.__vtestTrustedPageTransition('pageshow', true);
                console.log('restored');
                return { cleared: cleared, after: V.getConsoleLogs().map(function(l) { return l.message; }) };
                ",
            ),
        ],
    );
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = result(&results, 0);
    assert_eq!(r["console"], serde_json::json!(["before", "after"]), "{r}");
    assert_eq!(r["network"], 1, "{r}");
    assert_eq!(r["ipc"], 1, "{r}");
    assert_eq!(r["still_hooked"], true, "{r}");
    assert!(r["mutations"].as_u64().unwrap_or(0) >= 1, "{r}");
    let c = result(&results, 1);
    assert_eq!(c["cleared"], 0, "{c}");
    assert_eq!(c["after"], serde_json::json!(["restored"]), "{c}");
}
