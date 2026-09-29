//! Round-5 audit regressions for the injected JS bridge (jsdom via Node.js).
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
        setup_html: r#"<html lang="en"><head><title>R5</title></head><body><div id="app"></div></body></html>"#
            .to_string(),
        setup_js,
        tests,
    }
}

fn result(results: &[TestResult], i: usize) -> serde_json::Value {
    results[i].result.clone().unwrap_or_default()
}

// ── R5-JS1: log getters select + limit BEFORE deep-copying ───────────────────

/// Fill the IPC log with `n` completed calls whose response bodies are ~50 KB each (just under
/// the 64 KB capture cap), like a data-heavy app. Defines `__fillIpc` for the test code.
const FILL_IPC: &str = r"
window.__fillIpc = async function(n) {
    var item = { id: 1, title: 'x'.repeat(40), tags: ['a', 'b', 'c'], score: 0.5, nested: { k: 'v'.repeat(20) } };
    var rows = [];
    for (var i = 0; i < 380; i++) { var r = Object.assign({}, item); r.id = i; rows.push(r); }
    var big = JSON.stringify(rows);
    var ps = [];
    for (var j = 0; j < n; j++) {
        ps.push(fetch('http://ipc.localhost/get_items', {
            method: 'POST', body: JSON.stringify({ page: j }), headers: { 'x-vtest-body': big }
        }));
    }
    await Promise.all(ps);
    await new Promise(function(r) { setTimeout(r, 50); });
    return big.length;
};
";

/// `getIpcLog(limit)` deep-copied EVERY entry (args + ~50 KB result each) and only then kept
/// the last `limit`: ~860 ms per call on 1000 big entries, on the UI thread, for every
/// `logs ipc` / ghost / coverage / catalog / integrity read. It now selects first and copies
/// only what it returns. `waitForIpcComplete` read the full copied log to inspect one entry.
#[test]
fn r5_js1_ipc_log_reads_are_not_proportional_to_total_body_bytes() {
    let def = def(
        Some(FILL_IPC.to_string()),
        vec![case(
            "getIpcLog(100) / waitForIpcComplete on 1000 x 50 KB entries",
            r"
            var V = window.__VICTAURI__;
            var bodyBytes = await window.__fillIpc(1000);
            var t0 = performance.now();
            var last100 = V.getIpcLog(100);
            var limitedMs = performance.now() - t0;
            t0 = performance.now();
            var last10 = V.getIpcLog(10);
            var limited10Ms = performance.now() - t0;
            t0 = performance.now();
            var light = V.getIpcLog(0, { bodies: false });
            var lightMs = performance.now() - t0;
            t0 = performance.now();
            var done = await V.waitForIpcComplete(50);
            var waitMs = performance.now() - t0;
            // Still copies: mutating what was handed out must not reach the bridge's log.
            last100[99].result[0].title = 'forged';
            last100[99].args.page = -1;
            var again = V.getIpcLog(1)[0];
            return {
                body_bytes: bodyBytes,
                n: last100.length,
                first_page: last100[0].args.page,
                last_page: last100[99].args.page,
                result_rows: again.result.length,
                limited_ms: limitedMs,
                limited10_ms: limited10Ms,
                light_ms: lightMs,
                light_n: light.length,
                light_has_bodies: light.some(function(e) { return 'args' in e || 'result' in e; }),
                last10_first_page: last10[0].args.page,
                wait_ms: waitMs,
                wait_done: done,
                still_pristine: again.result[0].title !== 'forged' && again.args.page === 999,
            };
            ",
        )],
    );
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = result(&results, 0);
    eprintln!("R5-JS1 timings: {r}");
    assert!(r["body_bytes"].as_u64().unwrap() > 45_000, "{r}");
    assert_eq!(r["n"], 100, "{r}");
    assert_eq!(
        r["first_page"], 900,
        "limit must keep the NEWEST entries: {r}"
    );
    assert_eq!(r["result_rows"], 380, "{r}");
    assert_eq!(r["wait_done"], true, "{r}");
    assert_eq!(
        r["still_pristine"], true,
        "returned entries must be copies: {r}"
    );
    assert_eq!(r["last10_first_page"], 990, "{r}");
    assert_eq!(r["light_n"], 1000, "{r}");
    assert_eq!(r["light_has_bodies"], false, "{r}");
    // Before the fix each of these copied all ~50 MB first: ~1000 ms regardless of the limit.
    // Now the cost follows what is returned (100 bodies ~ 90 ms, 10 ~ 10 ms here); the bounds
    // leave several-fold headroom for a slow CI runner.
    let limited = r["limited_ms"].as_f64().unwrap();
    let limited10 = r["limited10_ms"].as_f64().unwrap();
    let light = r["light_ms"].as_f64().unwrap();
    let wait = r["wait_ms"].as_f64().unwrap();
    assert!(
        limited < 500.0,
        "getIpcLog(100) took {limited:.0} ms on 1000 big entries (must not copy them all): {r}"
    );
    assert!(
        limited10 < 100.0,
        "getIpcLog(10) took {limited10:.0} ms on 1000 big entries (must not copy them all): {r}"
    );
    assert!(
        light < 100.0,
        "getIpcLog(0, {{bodies:false}}) took {light:.0} ms (must not copy any body): {r}"
    );
    assert!(
        wait < 50.0,
        "waitForIpcComplete took {wait:.0} ms with the last call complete (must not copy the log): {r}"
    );
}

// ── R5-JS2: a page's throwing toJSON cannot make every eval hang ─────────────

/// A Tauri-like `invoke` stub: serializes the args with the page's (global, looked up at call
/// time) `JSON.stringify`, as Tauri's IPC transport does, and records the delivered result
/// per id. Plus the real eval scripts as `S.<name>` and `run(script)`.
fn eval_prelude(scripts: &[(&str, String)]) -> String {
    let mut js = String::from(
        r"
        var delivered = {};
        window.__TAURI_INTERNALS__ = { invoke: function(cmd, args) {
            var wire = JSON.stringify(args);
            var a = JSON.parse(wire);
            (delivered[a.id] = delivered[a.id] || []).push(a.result);
            return Promise.resolve(null);
        } };
        // A script's own promise rejecting (the wrapper escaping with an error) is recorded,
        // not left unhandled (which would abort the runner).
        var escaped = [];
        function run(s) {
            try {
                var p = (0, eval)(s);
                if (p && typeof p.then === 'function') p.then(null, function(e) { escaped.push('' + (e && e.message)); });
                return null;
            } catch (e) { return e.name; }
        }
        function sleep(ms) { return new Promise(function(r) { setTimeout(r, ms); }); }
        var S = {};
        ",
    );
    for (name, script) in scripts {
        js.push_str(&format!(
            "S[{}] = {};\n",
            serde_json::to_string(name).unwrap(),
            serde_json::to_string(script).unwrap()
        ));
    }
    js
}

/// Page script with a throwing `Object.prototype.toJSON` (a getter, or a method) broke every
/// serialization the eval plumbing did: `_evalSettle` marked the eval done, then its error
/// fallback — itself an object — threw again, so NO outcome was ever sent and every eval (and
/// the liveness probe) hung until the timeout. An outcome must always be delivered.
#[test]
fn r5_js2_throwing_to_json_cannot_suppress_eval_outcomes() {
    use victauri_plugin::js_bridge::{eval_probe_script, eval_wrapper_script};
    let prelude = eval_prelude(&[
        ("probe", eval_probe_script("id-p")),
        ("obj", eval_wrapper_script("id-obj", "return {a: 1}")),
        ("str", eval_wrapper_script("id-str", "return 'hello'")),
        (
            "thr",
            eval_wrapper_script("id-thr", "throw new Error('boom')"),
        ),
        ("undef", eval_wrapper_script("id-undef", "return undefined")),
    ]);
    let body = r"
        run(S.probe); run(S.obj); run(S.str); run(S.thr); run(S.undef);
        await sleep(30);
        delete Object.prototype.toJSON; // so the runner can report the result
        return { delivered: delivered, escaped: escaped };
    ";
    let getter = format!(
        "{prelude}\nObject.defineProperty(Object.prototype, 'toJSON', {{ configurable: true, \
         get: function() {{ throw new Error('nope'); }} }});\n{body}"
    );
    let method = format!(
        "{prelude}\nObject.prototype.toJSON = function() {{ throw new Error('nope'); }};\n{body}"
    );
    let def = def(
        None,
        vec![
            case("throwing toJSON getter", &getter),
            case("throwing toJSON method", &method),
        ],
    );
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    for i in 0..2 {
        let r = result(&results, i);
        assert_eq!(r["escaped"], serde_json::json!([]), "[{i}] {r}");
        let v = &r["delivered"];
        let one = |id: &str| -> serde_json::Value {
            let arr = v[id]
                .as_array()
                .unwrap_or_else(|| panic!("[{i}] no outcome delivered for {id}: {v}"));
            assert_eq!(arr.len(), 1, "[{i}] {id}: {v}");
            serde_json::from_str(arr[0].as_str().unwrap())
                .unwrap_or_else(|e| panic!("[{i}] {id} body is not JSON ({e}): {v}"))
        };
        assert!(
            one("id-p").as_str().unwrap().starts_with("probe_ok"),
            "[{i}] {v}"
        );
        assert_eq!(
            one("id-str"),
            serde_json::json!({ "__victauri_ok": "hello", "__victauri_type": "value" }),
            "[{i}]"
        );
        assert_eq!(
            one("id-undef"),
            serde_json::json!({ "__victauri_ok": null, "__victauri_type": "undefined" }),
            "[{i}]"
        );
        assert_eq!(
            one("id-thr"),
            serde_json::json!({ "__victauri_err": "boom" }),
            "[{i}]"
        );
        let obj = one("id-obj");
        assert!(
            obj["__victauri_unserializable"]
                .as_str()
                .is_some_and(|m| m.contains("nope")),
            "[{i}] an object result the page made unserializable is reported as such: {obj}"
        );
    }
}
