//! Round-5b audit regressions (Low findings) for the injected JS bridge (jsdom via Node.js).
//!
//! Same harness as `bridge_tests.rs` (`tests/bridge_tests/run_tests.js`): each test serializes
//! a batch of JS cases, runs them in a fresh jsdom with the real bridge injected, and asserts
//! on the results. Set `VICTAURI_REQUIRE_JSDOM=1` (CI sets `CI`) so a missing jsdom fails
//! instead of skipping.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use victauri_plugin::js_bridge::{
    BridgeCapacities, agent_op_call_js, eval_wrapper_script, init_script,
};

#[derive(Serialize)]
struct TestDef {
    bridge_script: String,
    setup_html: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    setup_js: Option<String>,
    tests: Vec<TestCase>,
}

#[derive(Serialize)]
struct TestCase {
    name: String,
    code: String,
    /// Expose Node's `Request` / `Response` / `Headers` to the page before the bridge loads.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    node_web_api: bool,
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
        node_web_api: false,
    }
}

fn web_case(name: &str, code: &str) -> TestCase {
    TestCase {
        node_web_api: true,
        ..case(name, code)
    }
}

fn def(setup_js: Option<String>, tests: Vec<TestCase>) -> TestDef {
    TestDef {
        bridge_script: init_script(&BridgeCapacities::default()),
        setup_html: r#"<html lang="en"><head><title>R5B</title></head><body><div id="app"></div></body></html>"#
            .to_string(),
        setup_js,
        tests,
    }
}

fn result(results: &[TestResult], i: usize) -> serde_json::Value {
    results[i].result.clone().unwrap_or_default()
}

/// Records every eval outcome delivered through a Tauri-like `invoke` stub, per id, parsed.
const DELIVERY_STUB: &str = r"
window.__delivered = {};
window.__TAURI_INTERNALS__ = { invoke: function(cmd, args) {
    var a = JSON.parse(JSON.stringify(args));
    try { window.__delivered[a.id] = JSON.parse(a.result); } catch (e) { window.__delivered[a.id] = a.result; }
    return Promise.resolve(null);
} };
window.__run = function(s) { (0, eval)(s); };
window.__sleep = function(ms) { return new Promise(function(r) { setTimeout(r, ms); }); };
";

fn scripts_js(scripts: &[(&str, String)]) -> String {
    let mut js = String::from("window.__S = {};\n");
    for (name, script) in scripts {
        js.push_str(&format!(
            "window.__S[{}] = {};\n",
            serde_json::to_string(name).unwrap(),
            serde_json::to_string(script).unwrap()
        ));
    }
    js
}

// ── R5B-SCRUBKEY1: animation scrub / sweep ops are agent-only ────────────────

/// `scrubPrepare` / `scrubSeek` / `scrubRestore` / `installSweepRecorder` / `readSweep` were on
/// the page-visible `window.__VICTAURI__`, so page script could erase or replace a sweep
/// recording (`installSweepRecorder` supersedes the agent's recorder, `readSweep(true)` clears
/// it) or drive a scrub. They now sit behind the agent key like the other agent-only ops, and
/// the exact snippets the `animation` tool sends still work.
#[test]
fn r5b_scrubkey1_animation_ops_are_agent_only() {
    let scripts = [
        (
            "arm",
            eval_wrapper_script("arm", &agent_op_call_js("installSweepRecorder(null)")),
        ),
        (
            "read",
            eval_wrapper_script("read", &agent_op_call_js("readSweep(false)")),
        ),
        (
            "prep",
            eval_wrapper_script("prep", &agent_op_call_js("scrubPrepare(null)")),
        ),
        (
            "seek",
            eval_wrapper_script("seek", &agent_op_call_js("scrubSeek(0.5)")),
        ),
        (
            "restore",
            eval_wrapper_script("restore", &agent_op_call_js("scrubRestore(true)")),
        ),
    ];
    let mut setup = String::from(DELIVERY_STUB);
    setup.push_str(&scripts_js(&scripts));
    let def = def(
        Some(setup),
        vec![case(
            "page script cannot reach the ops; the agent snippets can",
            r"
            var V = window.__VICTAURI__;
            var names = ['scrubPrepare', 'scrubSeek', 'scrubRestore', 'installSweepRecorder', 'readSweep'];
            var exposed = names.filter(function(n) { return n in V; });
            __run(__S.arm);
            await __sleep(30);
            __run(__S.read);
            __run(__S.prep);
            __run(__S.seek);
            __run(__S.restore);
            await __sleep(50);
            var d = window.__delivered;
            return {
                exposed: exposed,
                listed: typeof V.listAnimations,
                arm: d.arm && d.arm.__victauri_ok,
                read: d.read && d.read.__victauri_ok,
                prep: d.prep && d.prep.__victauri_ok,
                seek: d.seek && d.seek.__victauri_ok,
                restore: d.restore && d.restore.__victauri_ok,
            };
            ",
        )],
    );
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = result(&results, 0);
    assert_eq!(r["exposed"], serde_json::json!([]), "{r}");
    assert_eq!(
        r["listed"], "function",
        "`animation list` stays public: {r}"
    );
    assert_eq!(r["arm"]["installed"], true, "{r}");
    assert_eq!(r["read"]["armed"], true, "{r}");
    assert_eq!(r["read"]["session_count"], 0, "{r}");
    // jsdom runs no animations: prepare reports that, seek reports "not prepared".
    assert!(r["prep"]["error"].as_str().is_some(), "{r}");
    assert!(r["seek"]["error"].as_str().is_some(), "{r}");
    assert_eq!(r["restore"]["restored"], false, "{r}");
}

/// The moved ops run strict, so a page hook on a built-in they call cannot walk `.caller` to
/// an op (callable without the key) or to the injected script whose source holds the key.
#[test]
fn r5b_scrubkey1_page_hooks_never_reach_the_key_through_animation_ops() {
    let scripts = [
        (
            "arm",
            eval_wrapper_script("arm", &agent_op_call_js("installSweepRecorder('#app')")),
        ),
        (
            "read",
            eval_wrapper_script("read", &agent_op_call_js("readSweep(true)")),
        ),
        (
            "prep",
            eval_wrapper_script("prep", &agent_op_call_js("scrubPrepare('#app')")),
        ),
    ];
    let mut setup = String::from(DELIVERY_STUB);
    setup.push_str(&scripts_js(&scripts));
    setup.push_str(&format!(
        "window.__KEY = {};\n",
        serde_json::to_string(victauri_plugin::js_bridge::agent_key()).unwrap()
    ));
    let def = def(
        Some(setup),
        vec![case(
            "no hook sees a caller",
            r"
            var reach = [], calls = 0;
            function hook(obj, name) {
                var orig = obj[name];
                obj[name] = function h() {
                    calls++;
                    try {
                        for (var c = h.caller, n = 0; c && n < 20; c = c.caller, n++) {
                            reach.push(name + ':' + (('' + c).indexOf(window.__KEY) !== -1 ? 'KEY' : 'fn'));
                        }
                    } catch (e) {}
                    return orig.apply(this, arguments);
                };
            }
            hook(document, 'querySelector');
            hook(window, 'requestAnimationFrame');
            hook(Array.prototype, 'map');
            hook(Array.prototype, 'filter');
            hook(Promise, 'all');
            window.getComputedStyle = (function(o) { return function gcs(el) {
                try { if (gcs.caller) reach.push('gcs:fn'); } catch (e) {}
                return o.call(window, el);
            }; })(window.getComputedStyle);
            __run(__S.arm);
            await __sleep(60);
            __run(__S.read);
            __run(__S.prep);
            await __sleep(60);
            return { reach: reach, calls: calls, read: window.__delivered.read && window.__delivered.read.__victauri_ok };
            ",
        )],
    );
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = result(&results, 0);
    assert_eq!(r["reach"], serde_json::json!([]), "{r}");
    assert_eq!(r["read"]["armed"], true, "the ops really ran: {r}");
    assert!(
        r["calls"].as_u64().unwrap_or(0) > 0,
        "the hooks were exercised: {r}"
    );
}

// ── R5B-ROUTEURL1: a rule hits the same request however the app spells it ─────

/// A route pattern matched only the URL *as the app passed it*: `fetch('/api/x')` exposed the
/// relative string, a `Request` or `URL` object the absolute URL, so an `exact` (or anchored
/// regex) rule hit one form and silently missed the other. Rules now match the absolute URL,
/// the same-origin path, and the raw string.
#[test]
fn r5b_routeurl1_rules_match_relative_and_absolute_forms() {
    let def = def(
        None,
        vec![web_case(
            "exact / regex / glob rules against string, URL and Request forms, fetch and XHR",
            r"
            var V = window.__VICTAURI__;
            V.addRoute({ pattern: '/api/rel', match_type: 'exact', action: 'block' });
            V.addRoute({ pattern: 'http://localhost/api/abs', match_type: 'exact', action: 'block' });
            V.addRoute({ pattern: '^/api/rx', match_type: 'regex', action: 'block' });
            V.addRoute({ pattern: 'http://localhost/api/gl*', match_type: 'glob', action: 'block' });
            async function blocked(input) {
                try { await fetch(input); return false; }
                catch (e) { return /blocked by route/.test(e.message); }
            }
            var abs = function(p) { return 'http://localhost' + p; };
            var out = {};
            var paths = ['/api/rel', '/api/abs', '/api/rx', '/api/glob'];
            for (var i = 0; i < paths.length; i++) {
                var p = paths[i];
                out[p] = [
                    await blocked(p),
                    await blocked(abs(p)),
                    await blocked(new URL(p, location.href)),
                    await blocked(new Request(abs(p))),
                ];
            }
            // An unrelated path is untouched in every form.
            out.other = [await blocked('/api/other'), await blocked(new Request(abs('/api/other')))];
            // XHR, relative and absolute.
            function xhrBlocked(u) {
                return new Promise(function(res) {
                    var x = new XMLHttpRequest();
                    x.open('GET', u);
                    var done = function() { res(V.getNetworkLog(null, 1)[0].status === 'blocked'); };
                    x.addEventListener('error', done);
                    x.addEventListener('loadend', done);
                    x.send();
                });
            }
            out.xhr = [await xhrBlocked('/api/abs'), await xhrBlocked(abs('/api/rel'))];
            return out;
            ",
        )],
    );
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = result(&results, 0);
    for p in ["/api/rel", "/api/abs", "/api/rx", "/api/glob"] {
        assert_eq!(
            r[p],
            serde_json::json!([true, true, true, true]),
            "{p} must be blocked in every form: {r}"
        );
    }
    assert_eq!(r["other"], serde_json::json!([false, false]), "{r}");
    assert_eq!(r["xhr"], serde_json::json!([true, true]), "{r}");
}
