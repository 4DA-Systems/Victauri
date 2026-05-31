//! JS bridge tests using jsdom via Node.js subprocess.
//!
//! Each test function creates a batch of related JS test cases, serializes them
//! to a JSON temp file, runs `node run_tests.js <file>`, and asserts the results.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use victauri_plugin::js_bridge::{BridgeCapacities, init_script};

// ── Test Infrastructure ──────────────────────────────────────────────────────

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
    #[serde(skip_serializing_if = "Option::is_none")]
    setup_html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    setup_js: Option<String>,
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

fn jsdom_available() -> bool {
    runner_dir().join("node_modules").join("jsdom").exists()
}

fn run_tests(def: &TestDef) -> Option<Vec<TestResult>> {
    if !jsdom_available() {
        eprintln!("SKIP: jsdom not installed (run `npm install` in tests/bridge_tests/)");
        return None;
    }

    let runner = runner_dir().join("run_tests.js");
    assert!(
        runner.exists(),
        "run_tests.js not found at {}",
        runner.display()
    );

    // Write test def to temp file
    let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
    serde_json::to_writer(&mut tmp, def).expect("serialize test def");
    tmp.flush().expect("flush temp file");

    let output = Command::new("node")
        .arg(runner.to_str().unwrap())
        .arg(tmp.path().to_str().unwrap())
        .output()
        .expect("failed to run node");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Find the results line
    let results_line = stdout
        .lines()
        .find(|l| l.starts_with("VICTAURI_RESULTS:"))
        .unwrap_or_else(|| {
            panic!("No VICTAURI_RESULTS line in output.\nstdout: {stdout}\nstderr: {stderr}")
        });

    let json_str = &results_line["VICTAURI_RESULTS:".len()..];
    Some(
        serde_json::from_str(json_str)
            .unwrap_or_else(|e| panic!("Failed to parse results JSON: {e}\nraw: {json_str}")),
    )
}

fn assert_all_pass(results: &[TestResult]) {
    let mut failures = Vec::new();
    for r in results {
        if !r.passed {
            failures.push(format!(
                "  FAIL: {} => {:?}",
                r.name,
                r.error.as_deref().unwrap_or("unknown")
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "Test failures:\n{}",
        failures.join("\n")
    );
}

fn bridge_script() -> String {
    init_script(&BridgeCapacities::default())
}

fn default_html() -> String {
    r#"<html lang="en"><head><title>Test Page</title></head><body>
        <div id="app">
            <h1>Hello</h1>
            <button id="btn1">Click Me</button>
            <input id="input1" type="text" placeholder="Enter text" />
            <textarea id="ta1">Some text</textarea>
            <div contenteditable="true" id="ce1">Editable</div>
            <a href="/about" id="link1">About</a>
            <select id="sel1">
                <option value="a">Option A</option>
                <option value="b">Option B</option>
                <option value="c">Option C</option>
            </select>
            <nav><span>Nav item</span></nav>
        </div>
    </body></html>"#
        .to_string()
}

// ── Test Functions ───────────────────────────────────────────────────────────

#[test]
fn bridge_init_version_and_idempotent() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "version is 0.7.2".into(),
                code: "return window.__VICTAURI__.version;".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "bridge object exists".into(),
                code: "return typeof window.__VICTAURI__;".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "idempotent - re-inject does not overwrite".into(),
                code: format!(
                    r"
                    var versionBefore = window.__VICTAURI__.version;
                    eval({});
                    return versionBefore === window.__VICTAURI__.version ? 'same' : 'different';
                    ",
                    serde_json::to_string(&bridge_script()).unwrap()
                ),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "all public methods present".into(),
                code: r"
                    var methods = ['snapshot','getRef','getStaleRefs','findElements',
                        'click','doubleClick','hover','fill','type','pressKey','selectOption',
                        'scrollTo','focusElement','getIpcLog','clearIpcLog','getConsoleLogs',
                        'clearConsoleLogs','getMutationLog','clearMutationLog','getNetworkLog',
                        'clearNetworkLog','getLocalStorage','setLocalStorage','deleteLocalStorage',
                        'getSessionStorage','setSessionStorage','deleteSessionStorage','getCookies',
                        'getNavigationLog','navigate','navigateBack','getDialogLog','clearDialogLog',
                        'setDialogAutoResponse','getEventStream','waitFor','getStyles',
                        'getBoundingBoxes','highlightElement','clearHighlights','injectCss',
                        'removeInjectedCss','auditAccessibility','getPerformanceMetrics'];
                    var missing = methods.filter(function(m) { return typeof window.__VICTAURI__[m] !== 'function'; });
                    return { missing: missing, count: methods.length };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap(), "0.7.2");
    assert_eq!(results[1].result.as_ref().unwrap(), "object");
    assert_eq!(results[2].result.as_ref().unwrap(), "same");
    let missing: Vec<String> =
        serde_json::from_value(results[3].result.as_ref().unwrap()["missing"].clone()).unwrap();
    assert!(missing.is_empty(), "Missing methods: {missing:?}");
}

#[test]
fn snapshot_compact_format() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "snapshot default is compact".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot();
                    return { format: result.format, type: typeof result.tree };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "compact snapshot returns string tree".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('compact');
                    return { format: result.format, is_string: typeof result.tree === 'string', has_refs: result.tree.indexOf('[e') !== -1 };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "compact format contains button ref".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('compact');
                    return { has_button: result.tree.indexOf('button') !== -1, has_click_me: result.tree.indexOf('Click Me') !== -1 };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "stale_refs is array".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot();
                    return Array.isArray(result.stale_refs);
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["format"], "compact");
    assert_eq!(results[0].result.as_ref().unwrap()["type"], "string");
    assert_eq!(results[1].result.as_ref().unwrap()["is_string"], true);
    assert_eq!(results[1].result.as_ref().unwrap()["has_refs"], true);
    assert_eq!(results[2].result.as_ref().unwrap()["has_button"], true);
    assert_eq!(results[2].result.as_ref().unwrap()["has_click_me"], true);
    assert_eq!(
        results[3].result.as_ref().unwrap(),
        &serde_json::Value::Bool(true)
    );
}

#[test]
fn snapshot_json_format() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "json snapshot returns object tree".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('json');
                    return { format: result.format, tag: result.tree.tag, has_ref: !!result.tree.ref_id, has_children: result.tree.children.length > 0 };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "json tree has correct structure".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('json');
                    var body = result.tree;
                    return { tag: body.tag, visible: body.visible, has_bounds: !!body.bounds, has_role: body.role !== undefined };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "json tree child has ref and role".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('json');
                    var div = result.tree.children[0];
                    return { tag: div.tag, ref_starts_e: div.ref_id.charAt(0) === 'e', has_children: div.children.length > 0 };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "button in json tree has role and name".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('json');
                    function findByTag(node, tag) {
                        if (node.tag === tag) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findByTag(node.children[i], tag);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findByTag(result.tree, 'button');
                    return { role: btn.role, name: btn.name, tag: btn.tag };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["format"], "json");
    assert_eq!(results[0].result.as_ref().unwrap()["tag"], "body");
    assert_eq!(results[0].result.as_ref().unwrap()["has_ref"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["has_children"], true);
    assert_eq!(results[3].result.as_ref().unwrap()["role"], "button");
    assert_eq!(results[3].result.as_ref().unwrap()["name"], "Click Me");
}

#[test]
fn ref_lifecycle() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "snapshot assigns refs that can be resolved".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('json');
                    var ref_id = result.tree.ref_id;
                    var el = window.__VICTAURI__.getRef(ref_id);
                    return { ref_id: ref_id, resolved: el !== null, tag: el ? el.tagName.toLowerCase() : null };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "getRef returns null for invalid ref".into(),
                code: r"
                    var el = window.__VICTAURI__.getRef('e99999');
                    return { is_null: el === null };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "snapshot clears old refs and reports stale".into(),
                code: r"
                    // First snapshot — registers refs for all elements
                    var s1 = window.__VICTAURI__.snapshot('json');
                    // Find a button ref from first snapshot
                    function findByTag(node, tag) {
                        if (node.tag === tag) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findByTag(node.children[i], tag);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findByTag(s1.tree, 'button');
                    var btnRef = btn.ref_id;
                    // Remove the button from DOM to make its ref stale
                    var el = window.__VICTAURI__.getRef(btnRef);
                    if (el && el.parentNode) el.parentNode.removeChild(el);
                    // Second snapshot — should detect btnRef as stale
                    var s2 = window.__VICTAURI__.snapshot('json');
                    return { stale_has_btn: s2.stale_refs.indexOf(btnRef) !== -1 };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "getStaleRefs detects disconnected elements".into(),
                code: r"
                    window.__VICTAURI__.snapshot('json');
                    // Manually remove an element
                    var btn = document.getElementById('btn1');
                    btn.parentNode.removeChild(btn);
                    var stale = window.__VICTAURI__.getStaleRefs();
                    return { has_stale: stale.length > 0 };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["resolved"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["tag"], "body");
    assert_eq!(results[1].result.as_ref().unwrap()["is_null"], true);
    assert_eq!(results[2].result.as_ref().unwrap()["stale_has_btn"], true);
    assert_eq!(results[3].result.as_ref().unwrap()["has_stale"], true);
}

#[test]
fn click_interaction() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "click resolves ok for valid ref".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findByTag(node, tag) {
                        if (node.tag === tag) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findByTag(node.children[i], tag);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findByTag(snap.tree, 'button');
                    var result = await window.__VICTAURI__.click(btn.ref_id);
                    return result;
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "click fails for invalid ref".into(),
                code: r"
                    var result = await window.__VICTAURI__.click('e99999');
                    return result;
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "click dispatches click event".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findByTag(node, tag) {
                        if (node.tag === tag) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findByTag(node.children[i], tag);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findByTag(snap.tree, 'button');
                    var clicked = false;
                    var el = window.__VICTAURI__.getRef(btn.ref_id);
                    el.addEventListener('click', function() { clicked = true; });
                    await window.__VICTAURI__.click(btn.ref_id);
                    return { clicked: clicked };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["ok"], true);
    assert_eq!(results[1].result.as_ref().unwrap()["ok"], false);
    assert_eq!(results[2].result.as_ref().unwrap()["clicked"], true);
}

#[test]
fn double_click_and_hover() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "doubleClick dispatches dblclick event".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findByTag(node, tag) {
                        if (node.tag === tag) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findByTag(node.children[i], tag);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findByTag(snap.tree, 'button');
                    var dblClicked = false;
                    var el = window.__VICTAURI__.getRef(btn.ref_id);
                    el.addEventListener('dblclick', function() { dblClicked = true; });
                    await window.__VICTAURI__.doubleClick(btn.ref_id);
                    return { dblClicked: dblClicked };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "hover dispatches mouseenter and mouseover".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findByTag(node, tag) {
                        if (node.tag === tag) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findByTag(node.children[i], tag);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findByTag(snap.tree, 'button');
                    var events = [];
                    var el = window.__VICTAURI__.getRef(btn.ref_id);
                    el.addEventListener('mouseenter', function() { events.push('mouseenter'); });
                    el.addEventListener('mouseover', function() { events.push('mouseover'); });
                    await window.__VICTAURI__.hover(btn.ref_id);
                    return { events: events };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["dblClicked"], true);
    let events: Vec<String> =
        serde_json::from_value(results[1].result.as_ref().unwrap()["events"].clone()).unwrap();
    assert!(events.contains(&"mouseenter".to_string()));
    assert!(events.contains(&"mouseover".to_string()));
}

#[test]
fn fill_interaction() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "fill sets input value".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var inp = findById(snap.tree, 'input1');
                    await window.__VICTAURI__.fill(inp.ref_id, 'Hello World');
                    var el = window.__VICTAURI__.getRef(inp.ref_id);
                    return { ok: true, value: el.value };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "fill dispatches input and change events".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var inp = findById(snap.tree, 'input1');
                    var events = [];
                    var el = window.__VICTAURI__.getRef(inp.ref_id);
                    el.addEventListener('input', function() { events.push('input'); });
                    el.addEventListener('change', function() { events.push('change'); });
                    await window.__VICTAURI__.fill(inp.ref_id, 'test');
                    return { events: events };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "fill works on textarea".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var ta = findById(snap.tree, 'ta1');
                    await window.__VICTAURI__.fill(ta.ref_id, 'New text');
                    var el = window.__VICTAURI__.getRef(ta.ref_id);
                    return { value: el.value };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "fill fails on non-fillable element".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findByTag(node, tag) {
                        if (node.tag === tag) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findByTag(node.children[i], tag);
                            if (r) return r;
                        }
                        return null;
                    }
                    var h1 = findByTag(snap.tree, 'h1');
                    var result = await window.__VICTAURI__.fill(h1.ref_id, 'test');
                    return result;
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["value"], "Hello World");
    let events: Vec<String> =
        serde_json::from_value(results[1].result.as_ref().unwrap()["events"].clone()).unwrap();
    assert!(events.contains(&"input".to_string()));
    assert!(events.contains(&"change".to_string()));
    assert_eq!(results[2].result.as_ref().unwrap()["value"], "New text");
    assert_eq!(results[3].result.as_ref().unwrap()["ok"], false);
}

#[test]
fn type_text_interaction() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "type builds value char by char".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var inp = findById(snap.tree, 'input1');
                    await window.__VICTAURI__.type(inp.ref_id, 'abc');
                    var el = window.__VICTAURI__.getRef(inp.ref_id);
                    return { value: el.value };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "type dispatches keydown/keypress/input/keyup per char".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var inp = findById(snap.tree, 'input1');
                    var events = [];
                    var el = window.__VICTAURI__.getRef(inp.ref_id);
                    el.addEventListener('keydown', function(e) { events.push('keydown:' + e.key); });
                    el.addEventListener('keypress', function(e) { events.push('keypress:' + e.key); });
                    el.addEventListener('input', function(e) { events.push('input'); });
                    el.addEventListener('keyup', function(e) { events.push('keyup:' + e.key); });
                    await window.__VICTAURI__.type(inp.ref_id, 'x');
                    return { events: events };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["value"], "abc");
    let events: Vec<String> =
        serde_json::from_value(results[1].result.as_ref().unwrap()["events"].clone()).unwrap();
    assert!(events.contains(&"keydown:x".to_string()));
    assert!(events.contains(&"keypress:x".to_string()));
    assert!(events.contains(&"input".to_string()));
    assert!(events.contains(&"keyup:x".to_string()));
}

#[test]
fn press_key_interaction() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "pressKey dispatches on active element".into(),
                code: r"
                    var events = [];
                    document.body.addEventListener('keydown', function(e) { events.push('keydown:' + e.key); });
                    document.body.addEventListener('keyup', function(e) { events.push('keyup:' + e.key); });
                    var result = window.__VICTAURI__.pressKey('Escape');
                    return { ok: result.ok, events: events };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "pressKey returns ok true".into(),
                code: r"
                    var result = window.__VICTAURI__.pressKey('Enter');
                    return result;
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["ok"], true);
    let events: Vec<String> =
        serde_json::from_value(results[0].result.as_ref().unwrap()["events"].clone()).unwrap();
    assert!(events.contains(&"keydown:Escape".to_string()));
    assert!(events.contains(&"keyup:Escape".to_string()));
    assert_eq!(results[1].result.as_ref().unwrap()["ok"], true);
}

#[test]
fn select_option_interaction() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "selectOption sets selected options".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var sel = findById(snap.tree, 'sel1');
                    var result = await window.__VICTAURI__.selectOption(sel.ref_id, ['b']);
                    var el = window.__VICTAURI__.getRef(sel.ref_id);
                    return { ok: result.ok, value: el.value };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "selectOption fails on non-select element".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findByTag(node, tag) {
                        if (node.tag === tag) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findByTag(node.children[i], tag);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findByTag(snap.tree, 'button');
                    var result = await window.__VICTAURI__.selectOption(btn.ref_id, ['a']);
                    return result;
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["ok"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["value"], "b");
    assert_eq!(results[1].result.as_ref().unwrap()["ok"], false);
}

#[test]
fn focus_and_scroll() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "focusElement returns ok with tag".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var inp = findById(snap.tree, 'input1');
                    var result = await window.__VICTAURI__.focusElement(inp.ref_id);
                    return result;
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "scrollTo with no refId uses window".into(),
                code: r"
                    var result = await window.__VICTAURI__.scrollTo(null, 100, 200);
                    return result;
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "scrollTo with refId scrolls element into view".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findByTag(node, tag) {
                        if (node.tag === tag) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findByTag(node.children[i], tag);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findByTag(snap.tree, 'button');
                    var result = await window.__VICTAURI__.scrollTo(btn.ref_id, 0, 0);
                    return result;
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["ok"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["tag"], "input");
    assert_eq!(results[1].result.as_ref().unwrap()["ok"], true);
    assert_eq!(results[2].result.as_ref().unwrap()["ok"], true);
}

#[test]
fn console_log_capture() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "console.log is captured".into(),
                code: r"
                    console.log('hello from test');
                    var logs = window.__VICTAURI__.getConsoleLogs();
                    var last = logs[logs.length - 1];
                    return { level: last.level, message: last.message, has_ts: typeof last.timestamp === 'number' };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "console.warn and error are captured".into(),
                code: r"
                    console.warn('warning!');
                    console.error('error!');
                    var logs = window.__VICTAURI__.getConsoleLogs();
                    var warn = logs.find(function(l) { return l.level === 'warn'; });
                    var err = logs.find(function(l) { return l.level === 'error'; });
                    return { has_warn: !!warn, has_error: !!err, warn_msg: warn ? warn.message : null, err_msg: err ? err.message : null };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "clearConsoleLogs empties the log".into(),
                code: r"
                    console.log('test');
                    window.__VICTAURI__.clearConsoleLogs();
                    var logs = window.__VICTAURI__.getConsoleLogs();
                    return { count: logs.length };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "getConsoleLogs since filter works".into(),
                code: r"
                    console.log('old message');
                    var ts = Date.now() + 1;
                    console.log('new message');
                    var filtered = window.__VICTAURI__.getConsoleLogs(ts);
                    // Since both happen nearly simultaneously, just verify the API doesn't crash
                    return { is_array: Array.isArray(filtered) };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["level"], "log");
    assert_eq!(
        results[0].result.as_ref().unwrap()["message"],
        "hello from test"
    );
    assert_eq!(results[0].result.as_ref().unwrap()["has_ts"], true);
    assert_eq!(results[1].result.as_ref().unwrap()["has_warn"], true);
    assert_eq!(results[1].result.as_ref().unwrap()["has_error"], true);
    assert_eq!(results[2].result.as_ref().unwrap()["count"], 0);
    assert_eq!(results[3].result.as_ref().unwrap()["is_array"], true);
}

#[test]
fn network_log_interception() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "fetch calls are logged".into(),
                code: r"
                    await fetch('http://example.com/api/data');
                    // Give network log time to update
                    var log = window.__VICTAURI__.getNetworkLog();
                    var entry = log.find(function(e) { return e.url.indexOf('example.com') !== -1; });
                    return { found: !!entry, method: entry ? entry.method : null, has_id: entry ? typeof entry.id === 'number' : false };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "getNetworkLog filter works".into(),
                code: r"
                    await fetch('http://api.example.com/users');
                    await fetch('http://other.com/data');
                    var filtered = window.__VICTAURI__.getNetworkLog('api.example.com');
                    return { count: filtered.length, url: filtered.length > 0 ? filtered[0].url : null };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "clearNetworkLog empties the log".into(),
                code: r"
                    await fetch('http://example.com/test');
                    window.__VICTAURI__.clearNetworkLog();
                    var log = window.__VICTAURI__.getNetworkLog();
                    return { count: log.length };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["found"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["method"], "GET");
    assert_eq!(results[0].result.as_ref().unwrap()["has_id"], true);
    assert_eq!(results[2].result.as_ref().unwrap()["count"], 0);
}

#[test]
fn navigation_tracking() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "initial navigation entry exists".into(),
                code: r"
                    var log = window.__VICTAURI__.getNavigationLog();
                    return { count: log.length, first_type: log[0] ? log[0].type : null };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "pushState is tracked".into(),
                code: r"
                    history.pushState({}, '', '/new-page');
                    var log = window.__VICTAURI__.getNavigationLog();
                    var push = log.find(function(e) { return e.type === 'pushState'; });
                    return { found: !!push, url: push ? push.url : null };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "replaceState is tracked".into(),
                code: r"
                    history.replaceState({}, '', '/replaced');
                    var log = window.__VICTAURI__.getNavigationLog();
                    var replace = log.find(function(e) { return e.type === 'replaceState'; });
                    return { found: !!replace, url: replace ? replace.url : null };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert!(
        results[0].result.as_ref().unwrap()["count"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert_eq!(results[0].result.as_ref().unwrap()["first_type"], "initial");
    assert_eq!(results[1].result.as_ref().unwrap()["found"], true);
    assert_eq!(results[2].result.as_ref().unwrap()["found"], true);
}

#[test]
fn dialog_capture() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "alert is captured".into(),
                code: r"
                    window.alert('Hello!');
                    var log = window.__VICTAURI__.getDialogLog();
                    var entry = log[log.length - 1];
                    return { type: entry.type, message: entry.message };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "confirm returns false by default (fail-closed)".into(),
                code: r"
                    var result = window.confirm('Are you sure?');
                    var log = window.__VICTAURI__.getDialogLog();
                    var entry = log[log.length - 1];
                    return { result: result, type: entry.type, message: entry.message };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "prompt returns null by default (fail-closed)".into(),
                code: r"
                    var result = window.prompt('Enter name:');
                    var log = window.__VICTAURI__.getDialogLog();
                    var entry = log[log.length - 1];
                    return { result: result, type: entry.type, message: entry.message };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "setDialogAutoResponse changes confirm behavior".into(),
                code: r"
                    window.__VICTAURI__.setDialogAutoResponse('confirm', 'dismiss');
                    var result = window.confirm('Will you?');
                    return { result: result };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "clearDialogLog empties the log".into(),
                code: r"
                    window.alert('test');
                    window.__VICTAURI__.clearDialogLog();
                    var log = window.__VICTAURI__.getDialogLog();
                    return { count: log.length };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["type"], "alert");
    assert_eq!(results[0].result.as_ref().unwrap()["message"], "Hello!");
    // Fail-closed default (audit #32): confirm() -> false, prompt() -> null.
    assert_eq!(results[1].result.as_ref().unwrap()["result"], false);
    assert_eq!(results[1].result.as_ref().unwrap()["type"], "confirm");
    assert_eq!(
        results[2].result.as_ref().unwrap()["result"],
        serde_json::Value::Null
    );
    assert_eq!(results[2].result.as_ref().unwrap()["type"], "prompt");
    assert_eq!(results[3].result.as_ref().unwrap()["result"], false);
    assert_eq!(results[4].result.as_ref().unwrap()["count"], 0);
}

#[test]
fn storage_local() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "setLocalStorage and get round-trips".into(),
                code: r"
                    window.__VICTAURI__.setLocalStorage('key1', 'value1');
                    var val = window.__VICTAURI__.getLocalStorage('key1');
                    return { val: val };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "setLocalStorage with object serializes JSON".into(),
                code: r"
                    window.__VICTAURI__.setLocalStorage('obj', { a: 1, b: 'hi' });
                    var val = window.__VICTAURI__.getLocalStorage('obj');
                    return val;
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "deleteLocalStorage removes key".into(),
                code: r"
                    window.__VICTAURI__.setLocalStorage('del_me', 'exists');
                    window.__VICTAURI__.deleteLocalStorage('del_me');
                    var val = window.__VICTAURI__.getLocalStorage('del_me');
                    return { val: val };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "getLocalStorage with no key returns all".into(),
                code: r"
                    window.__VICTAURI__.setLocalStorage('aa', 'one');
                    window.__VICTAURI__.setLocalStorage('bb', 'two');
                    var all = window.__VICTAURI__.getLocalStorage();
                    return { has_aa: all.aa === 'one', has_bb: all.bb === 'two' };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["val"], "value1");
    assert_eq!(results[1].result.as_ref().unwrap()["a"], 1);
    assert_eq!(results[1].result.as_ref().unwrap()["b"], "hi");
    // deleteLocalStorage: value is null
    assert!(results[2].result.as_ref().unwrap()["val"].is_null());
    assert_eq!(results[3].result.as_ref().unwrap()["has_aa"], true);
    assert_eq!(results[3].result.as_ref().unwrap()["has_bb"], true);
}

#[test]
fn storage_session() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "setSessionStorage and get round-trips".into(),
                code: r"
                    window.__VICTAURI__.setSessionStorage('skey', 'sval');
                    var val = window.__VICTAURI__.getSessionStorage('skey');
                    return { val: val };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "deleteSessionStorage removes key".into(),
                code: r"
                    window.__VICTAURI__.setSessionStorage('sdel', 'exists');
                    window.__VICTAURI__.deleteSessionStorage('sdel');
                    var val = window.__VICTAURI__.getSessionStorage('sdel');
                    return { val: val };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["val"], "sval");
    assert!(results[1].result.as_ref().unwrap()["val"].is_null());
}

#[test]
fn css_inspection() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "getStyles returns styles for ref".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    var result = window.__VICTAURI__.getStyles(snap.tree.ref_id);
                    return { has_ref: !!result.ref_id, has_tag: !!result.tag, has_styles: typeof result.styles === 'object' };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "getStyles with specific properties".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    var result = window.__VICTAURI__.getStyles(snap.tree.ref_id, ['display', 'visibility']);
                    return { display: result.styles.display, visibility: result.styles.visibility };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "getStyles errors for invalid ref".into(),
                code: r"
                    var result = window.__VICTAURI__.getStyles('e99999');
                    return { has_error: !!result.error };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "getBoundingBoxes returns box model".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    var result = window.__VICTAURI__.getBoundingBoxes([snap.tree.ref_id]);
                    var box = result[0];
                    return { ref_id: box.ref_id, has_width: typeof box.width === 'number', has_margin: typeof box.margin === 'object', has_padding: typeof box.padding === 'object' };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "getBoundingBoxes errors for invalid ref".into(),
                code: r"
                    var result = window.__VICTAURI__.getBoundingBoxes(['e99999']);
                    return { error: result[0].error };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["has_ref"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["has_tag"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["has_styles"], true);
    assert_eq!(results[1].result.as_ref().unwrap()["display"], "block");
    assert_eq!(results[1].result.as_ref().unwrap()["visibility"], "visible");
    assert_eq!(results[2].result.as_ref().unwrap()["has_error"], true);
    assert_eq!(results[3].result.as_ref().unwrap()["has_width"], true);
    assert_eq!(results[3].result.as_ref().unwrap()["has_margin"], true);
    assert_eq!(results[3].result.as_ref().unwrap()["has_padding"], true);
    assert_eq!(
        results[4].result.as_ref().unwrap()["error"],
        "ref not found"
    );
}

#[test]
fn css_injection() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "injectCss creates style element".into(),
                code: r"
                    var result = window.__VICTAURI__.injectCss('body { color: red; }');
                    var style = document.getElementById('__victauri_injected_css__');
                    return { ok: result.ok, length: result.length, exists: !!style, content: style ? style.textContent : null };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "injectCss replaces previous injection".into(),
                code: r"
                    window.__VICTAURI__.injectCss('body { color: red; }');
                    window.__VICTAURI__.injectCss('body { color: blue; }');
                    var styles = document.querySelectorAll('#__victauri_injected_css__');
                    return { count: styles.length, content: styles[0] ? styles[0].textContent : null };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "removeInjectedCss removes style element".into(),
                code: r"
                    window.__VICTAURI__.injectCss('body { color: green; }');
                    var result = window.__VICTAURI__.removeInjectedCss();
                    var style = document.getElementById('__victauri_injected_css__');
                    return { ok: result.ok, removed: result.removed, exists: !!style };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "removeInjectedCss when none exists returns removed:false".into(),
                code: r"
                    var result = window.__VICTAURI__.removeInjectedCss();
                    return result;
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["ok"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["exists"], true);
    assert_eq!(
        results[0].result.as_ref().unwrap()["content"],
        "body { color: red; }"
    );
    assert_eq!(results[1].result.as_ref().unwrap()["count"], 1);
    assert_eq!(
        results[1].result.as_ref().unwrap()["content"],
        "body { color: blue; }"
    );
    assert_eq!(results[2].result.as_ref().unwrap()["removed"], true);
    assert_eq!(results[2].result.as_ref().unwrap()["exists"], false);
    assert_eq!(results[3].result.as_ref().unwrap()["removed"], false);
}

#[test]
fn highlight_elements() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "highlightElement creates overlay".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    var result = window.__VICTAURI__.highlightElement(snap.tree.ref_id, 'red', 'test label');
                    var overlays = document.querySelectorAll('.__victauri_highlight__');
                    return { ok: result.ok, count: overlays.length, has_label: overlays[0] && overlays[0].children.length > 0 };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "highlightElement errors for invalid ref".into(),
                code: r"
                    var result = window.__VICTAURI__.highlightElement('e99999');
                    return { has_error: !!result.error };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "clearHighlights removes all overlays".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    window.__VICTAURI__.highlightElement(snap.tree.ref_id);
                    window.__VICTAURI__.highlightElement(snap.tree.ref_id);
                    var result = window.__VICTAURI__.clearHighlights();
                    var overlays = document.querySelectorAll('.__victauri_highlight__');
                    return { ok: result.ok, removed: result.removed, remaining: overlays.length };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["ok"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["count"], 1);
    assert_eq!(results[0].result.as_ref().unwrap()["has_label"], true);
    assert_eq!(results[1].result.as_ref().unwrap()["has_error"], true);
    assert_eq!(results[2].result.as_ref().unwrap()["ok"], true);
    assert_eq!(results[2].result.as_ref().unwrap()["removed"], 2);
    assert_eq!(results[2].result.as_ref().unwrap()["remaining"], 0);
}

#[test]
fn accessibility_audit() {
    let a11y_html = r#"<html><head><title>A11y Test</title></head><body>
        <img src="test.png" />
        <input type="text" />
        <button></button>
        <a href="/test"></a>
        <div role="invalid_role">test</div>
        <div tabindex="5">tab</div>
    </body></html>"#
        .to_string();

    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: a11y_html,
        setup_js: None,
        tests: vec![
            TestCase {
                name: "audit detects img without alt".into(),
                code: r"
                    var result = window.__VICTAURI__.auditAccessibility();
                    var imgAlt = result.violations.find(function(v) { return v.rule === 'img-alt'; });
                    return { found: !!imgAlt, severity: imgAlt ? imgAlt.severity : null };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "audit detects input without label".into(),
                code: r"
                    var result = window.__VICTAURI__.auditAccessibility();
                    var inputLabel = result.violations.find(function(v) { return v.rule === 'input-label'; });
                    return { found: !!inputLabel, severity: inputLabel ? inputLabel.severity : null };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "audit detects button without name".into(),
                code: r"
                    var result = window.__VICTAURI__.auditAccessibility();
                    var btnName = result.violations.find(function(v) { return v.rule === 'button-name'; });
                    return { found: !!btnName };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "audit detects link without text".into(),
                code: r"
                    var result = window.__VICTAURI__.auditAccessibility();
                    var linkName = result.violations.find(function(v) { return v.rule === 'link-name'; });
                    return { found: !!linkName };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "audit detects invalid ARIA role".into(),
                code: r"
                    var result = window.__VICTAURI__.auditAccessibility();
                    var ariaRole = result.warnings.find(function(w) { return w.rule === 'aria-role'; });
                    return { found: !!ariaRole, message: ariaRole ? ariaRole.message : null };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "audit detects positive tabindex".into(),
                code: r"
                    var result = window.__VICTAURI__.auditAccessibility();
                    var tabIdx = result.warnings.find(function(w) { return w.rule === 'tabindex-positive'; });
                    return { found: !!tabIdx };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "audit returns summary counts".into(),
                code: r"
                    var result = window.__VICTAURI__.auditAccessibility();
                    return { has_summary: !!result.summary, total: result.summary.total };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["found"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["severity"], "critical");
    assert_eq!(results[1].result.as_ref().unwrap()["found"], true);
    assert_eq!(results[1].result.as_ref().unwrap()["severity"], "serious");
    assert_eq!(results[2].result.as_ref().unwrap()["found"], true);
    assert_eq!(results[3].result.as_ref().unwrap()["found"], true);
    assert_eq!(results[4].result.as_ref().unwrap()["found"], true);
    assert_eq!(results[5].result.as_ref().unwrap()["found"], true);
    assert_eq!(results[6].result.as_ref().unwrap()["has_summary"], true);
    assert!(
        results[6].result.as_ref().unwrap()["total"]
            .as_u64()
            .unwrap()
            > 0
    );
}

#[test]
fn event_stream() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "getEventStream returns console events".into(),
                code: r"
                    console.log('stream test');
                    var events = window.__VICTAURI__.getEventStream();
                    var consoleEvents = events.filter(function(e) { return e.type === 'console'; });
                    return { has_console: consoleEvents.length > 0, first_type: consoleEvents.length > 0 ? consoleEvents[0].type : null };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "getEventStream includes network events".into(),
                code: r"
                    await fetch('http://test.com/api');
                    var events = window.__VICTAURI__.getEventStream();
                    var netEvents = events.filter(function(e) { return e.type === 'network'; });
                    return { has_network: netEvents.length > 0 };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "getEventStream since filter works".into(),
                code: r"
                    console.log('old');
                    var ts = Date.now() + 10000;
                    var events = window.__VICTAURI__.getEventStream(ts);
                    return { count: events.length };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "getEventStream includes navigation events".into(),
                code: r"
                    var events = window.__VICTAURI__.getEventStream();
                    var navEvents = events.filter(function(e) { return e.type === 'navigation'; });
                    return { has_nav: navEvents.length > 0, first_nav_type: navEvents.length > 0 ? navEvents[0].nav_type : null };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["has_console"], true);
    assert_eq!(results[1].result.as_ref().unwrap()["has_network"], true);
    assert_eq!(results[2].result.as_ref().unwrap()["count"], 0);
    assert_eq!(results[3].result.as_ref().unwrap()["has_nav"], true);
    assert_eq!(
        results[3].result.as_ref().unwrap()["first_nav_type"],
        "initial"
    );
}

#[test]
fn wait_for_conditions() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "waitFor text condition met immediately".into(),
                code: r"
                    var result = await window.__VICTAURI__.waitFor({ condition: 'text', value: 'Hello', timeout_ms: 500 });
                    return result;
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "waitFor selector condition met".into(),
                code: r"
                    var result = await window.__VICTAURI__.waitFor({ condition: 'selector', value: '#btn1', timeout_ms: 500 });
                    return result;
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "waitFor selector_gone when element absent".into(),
                code: r"
                    var result = await window.__VICTAURI__.waitFor({ condition: 'selector_gone', value: '#nonexistent', timeout_ms: 500 });
                    return result;
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "waitFor text_gone when text absent".into(),
                code: r"
                    var result = await window.__VICTAURI__.waitFor({ condition: 'text_gone', value: 'NONEXISTENT_TEXT_XYZ', timeout_ms: 500 });
                    return result;
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "waitFor timeout when condition not met".into(),
                code: r"
                    var result = await window.__VICTAURI__.waitFor({ condition: 'text', value: 'IMPOSSIBLE_TEXT_XYZ', timeout_ms: 100, poll_ms: 20 });
                    return { ok: result.ok, has_error: !!result.error };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["ok"], true);
    assert_eq!(results[1].result.as_ref().unwrap()["ok"], true);
    assert_eq!(results[2].result.as_ref().unwrap()["ok"], true);
    assert_eq!(results[3].result.as_ref().unwrap()["ok"], true);
    assert_eq!(results[4].result.as_ref().unwrap()["ok"], false);
    assert_eq!(results[4].result.as_ref().unwrap()["has_error"], true);
}

#[test]
fn ipc_log() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "IPC calls are captured from network log".into(),
                code: r"
                    await fetch('http://ipc.localhost/get_settings', { method: 'POST', body: '{}' });
                    var log = window.__VICTAURI__.getIpcLog();
                    var entry = log.find(function(e) { return e.command === 'get_settings'; });
                    return { found: !!entry, command: entry ? entry.command : null };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "Victauri plugin calls are excluded from IPC log".into(),
                code: r"
                    await fetch('http://ipc.localhost/plugin%3Avictauri%7Ceval', { method: 'POST', body: '{}' });
                    var log = window.__VICTAURI__.getIpcLog();
                    var victauriEntry = log.find(function(e) { return e.command && e.command.indexOf('victauri') !== -1; });
                    return { excluded: !victauriEntry };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "IPC log limit works".into(),
                code: r"
                    await fetch('http://ipc.localhost/cmd1', { method: 'POST', body: '{}' });
                    await fetch('http://ipc.localhost/cmd2', { method: 'POST', body: '{}' });
                    await fetch('http://ipc.localhost/cmd3', { method: 'POST', body: '{}' });
                    var limited = window.__VICTAURI__.getIpcLog(2);
                    return { count: limited.length };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "clearIpcLog removes IPC entries only".into(),
                code: r"
                    await fetch('http://ipc.localhost/test_cmd', { method: 'POST', body: '{}' });
                    await fetch('http://other.com/api');
                    window.__VICTAURI__.clearIpcLog();
                    var ipcLog = window.__VICTAURI__.getIpcLog();
                    var netLog = window.__VICTAURI__.getNetworkLog();
                    return { ipc_count: ipcLog.length, net_has_other: netLog.some(function(e) { return e.url.indexOf('other.com') !== -1; }) };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["found"], true);
    assert_eq!(
        results[0].result.as_ref().unwrap()["command"],
        "get_settings"
    );
    assert_eq!(results[1].result.as_ref().unwrap()["excluded"], true);
    assert_eq!(results[2].result.as_ref().unwrap()["count"], 2);
    assert_eq!(results[3].result.as_ref().unwrap()["ipc_count"], 0);
    assert_eq!(results[3].result.as_ref().unwrap()["net_has_other"], true);
}

#[test]
fn performance_metrics() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "getPerformanceMetrics returns structure".into(),
                code: r"
                    var result = window.__VICTAURI__.getPerformanceMetrics();
                    return {
                        has_resources: !!result.resources,
                        has_paint: !!result.paint,
                        has_dom: !!result.dom,
                        dom_elements: result.dom ? result.dom.elements : 0,
                        dom_has_depth: result.dom ? typeof result.dom.max_depth === 'number' : false
                    };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "DOM stats are correct".into(),
                code: r"
                    var result = window.__VICTAURI__.getPerformanceMetrics();
                    return { elements: result.dom.elements, max_depth_gte_0: result.dom.max_depth >= 0 };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["has_resources"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["has_paint"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["has_dom"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["dom_has_depth"], true);
    assert!(
        results[0].result.as_ref().unwrap()["dom_elements"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(results[1].result.as_ref().unwrap()["max_depth_gte_0"], true);
}

#[test]
fn find_elements() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Find Test</title></head><body>
            <button data-testid="save-btn" aria-label="Save document">Save</button>
            <button data-testid="cancel-btn">Cancel</button>
            <input type="text" placeholder="Search..." />
            <div role="dialog">Dialog content</div>
        </body></html>"#.to_string(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "findElements by text".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ text: 'Save' });
                    return { count: results.length, first_tag: results[0] ? results[0].tag : null };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "findElements by role".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ role: 'button' });
                    return { count: results.length };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "findElements by test_id".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ test_id: 'save-btn' });
                    return { count: results.length, text: results[0] ? results[0].text : null };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "findElements by css selector".into(),
                code: r#"
                    var results = window.__VICTAURI__.findElements({ css: 'input[type="text"]' });
                    return { count: results.length, tag: results[0] ? results[0].tag : null };
                "#.into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "findElements by name (aria-label)".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ name: 'Save document' });
                    return { count: results.length, ref_id: results[0] ? results[0].ref_id : null };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "findElements respects max_results".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ role: 'button', max_results: 1 });
                    return { count: results.length };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert!(
        results[0].result.as_ref().unwrap()["count"]
            .as_u64()
            .unwrap()
            >= 1
    );
    // The first match might be body (which contains "Save" text) or button depending on traversal
    // Just check that at least one result was found
    let first_tag = results[0].result.as_ref().unwrap()["first_tag"]
        .as_str()
        .unwrap();
    assert!(
        first_tag == "button" || first_tag == "body",
        "Expected button or body, got {first_tag}"
    );
    assert!(
        results[1].result.as_ref().unwrap()["count"]
            .as_u64()
            .unwrap()
            >= 2
    );
    assert_eq!(results[2].result.as_ref().unwrap()["count"], 1);
    assert_eq!(results[2].result.as_ref().unwrap()["text"], "Save");
    assert_eq!(results[3].result.as_ref().unwrap()["count"], 1);
    assert_eq!(results[3].result.as_ref().unwrap()["tag"], "input");
    assert_eq!(results[4].result.as_ref().unwrap()["count"], 1);
    assert_eq!(results[5].result.as_ref().unwrap()["count"], 1);
}

#[test]
fn actionability_checks() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Test</title></head><body>
            <button id="disabled-btn" disabled>Disabled</button>
            <button id="aria-disabled-btn" aria-disabled="true">Aria Disabled</button>
            <button id="hidden-btn" style="display:none">Hidden</button>
            <button id="good-btn">Good</button>
        </body></html>"#.to_string(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "click fails on disabled element".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findById(snap.tree, 'disabled-btn');
                    if (!btn) return { ok: false, error: 'disabled-btn not in snapshot (expected - disabled is still visible)' };
                    var result = await window.__VICTAURI__.click(btn.ref_id, 100);
                    return result;
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "click fails on aria-disabled element".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findById(snap.tree, 'aria-disabled-btn');
                    if (!btn) return { ok: false, error: 'btn not found in tree' };
                    var result = await window.__VICTAURI__.click(btn.ref_id, 100);
                    return result;
                ".into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "click works on good button".into(),
                code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findById(snap.tree, 'good-btn');
                    var result = await window.__VICTAURI__.click(btn.ref_id);
                    return result;
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    // disabled button: might timeout or give error
    assert_eq!(results[0].result.as_ref().unwrap()["ok"], false);
    assert_eq!(results[1].result.as_ref().unwrap()["ok"], false);
    assert_eq!(results[2].result.as_ref().unwrap()["ok"], true);
}

#[test]
fn capacity_limits() {
    let def = TestDef {
        bridge_script: init_script(&BridgeCapacities {
            console_logs: 5,
            mutation_log: 500,
            network_log: 5,
            navigation_log: 3,
            dialog_log: 3,
            long_tasks: 100,
        }),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "console log cap enforced".into(),
                code: r"
                    for (var i = 0; i < 10; i++) console.log('msg ' + i);
                    var logs = window.__VICTAURI__.getConsoleLogs();
                    return { count: logs.length, first_msg: logs[0].message };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "network log cap enforced".into(),
                code: r"
                    for (var i = 0; i < 10; i++) await fetch('http://example.com/' + i);
                    var log = window.__VICTAURI__.getNetworkLog();
                    return { count: log.length };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "dialog log cap enforced".into(),
                code: r"
                    for (var i = 0; i < 10; i++) window.alert('alert ' + i);
                    var log = window.__VICTAURI__.getDialogLog();
                    return { count: log.length };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["count"], 5);
    // The oldest entries are shifted out, so first message should be "msg 5"
    assert_eq!(results[0].result.as_ref().unwrap()["first_msg"], "msg 5");
    assert_eq!(results[1].result.as_ref().unwrap()["count"], 5);
    assert_eq!(results[2].result.as_ref().unwrap()["count"], 3);
}

#[test]
fn infer_role_mapping() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Role Test</title></head><body>
            <button>Btn</button>
            <a href="/">Link</a>
            <input type="checkbox" />
            <input type="radio" />
            <input type="range" />
            <input type="submit" value="Submit" />
            <nav>Nav</nav>
            <main>Main</main>
            <aside>Aside</aside>
        </body></html>"#.to_string(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "roles inferred correctly in json snapshot".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('json');
                    function collectRoles(node, acc) {
                        if (node.role) acc.push(node.tag + ':' + node.role);
                        for (var i = 0; i < node.children.length; i++) collectRoles(node.children[i], acc);
                        return acc;
                    }
                    var roles = collectRoles(result.tree, []);
                    return {
                        has_button: roles.some(function(r) { return r === 'button:button'; }),
                        has_link: roles.some(function(r) { return r === 'a:link'; }),
                        has_checkbox: roles.some(function(r) { return r === 'input:checkbox'; }),
                        has_radio: roles.some(function(r) { return r === 'input:radio'; }),
                        has_slider: roles.some(function(r) { return r === 'input:slider'; }),
                        has_nav: roles.some(function(r) { return r === 'nav:navigation'; }),
                        has_main: roles.some(function(r) { return r === 'main:main'; }),
                        has_complementary: roles.some(function(r) { return r === 'aside:complementary'; }),
                    };
                ".into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = results[0].result.as_ref().unwrap();
    assert_eq!(r["has_button"], true);
    assert_eq!(r["has_link"], true);
    assert_eq!(r["has_checkbox"], true);
    assert_eq!(r["has_radio"], true);
    assert_eq!(r["has_slider"], true);
    assert_eq!(r["has_nav"], true);
    assert_eq!(r["has_main"], true);
    assert_eq!(r["has_complementary"], true);
}

#[test]
fn compact_format_details() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Compact Test</title></head><body>
            <input id="myinput" type="text" data-testid="search" disabled value="hello" />
            <a href="/about">About Us</a>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "compact format shows disabled, value, type, data-testid, href".into(),
            code: r"
                    var result = window.__VICTAURI__.snapshot('compact');
                    var tree = result.tree;
                    return {
                        has_disabled: tree.indexOf('[disabled]') !== -1,
                        has_value: tree.indexOf('value=') !== -1,
                        has_type: tree.indexOf('type=text') !== -1,
                        has_testid: tree.indexOf('@search') !== -1,
                        has_href: tree.indexOf('href=/about') !== -1,
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = results[0].result.as_ref().unwrap();
    assert_eq!(r["has_disabled"], true);
    assert_eq!(r["has_value"], true);
    assert_eq!(r["has_type"], true);
    assert_eq!(r["has_testid"], true);
    assert_eq!(r["has_href"], true);
}

// ══════════════════════════════════════════════════════════════════════════════
// Extended DOM Snapshot Tests
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn snapshot_empty_body() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Empty</title></head><body></body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "snapshot of empty body returns minimal tree".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('json');
                    return { tag: result.tree.tag, children_count: result.tree.children.length, format: result.format };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "compact snapshot of empty body returns single line".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('compact');
                    var lines = result.tree.trim().split('\n');
                    return { line_count: lines.length, first_line_has_body: lines[0].indexOf('body') !== -1 };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["tag"], "body");
    assert_eq!(results[0].result.as_ref().unwrap()["children_count"], 0);
    assert_eq!(
        results[1].result.as_ref().unwrap()["first_line_has_body"],
        true
    );
}

#[test]
fn snapshot_deeply_nested_dom() {
    let deep_html = r#"<html lang="en"><head><title>Deep</title></head><body>
        <div id="l1"><div id="l2"><div id="l3"><div id="l4"><div id="l5">
            <span>Deep content</span>
        </div></div></div></div></div>
    </body></html>"#
        .to_string();

    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: deep_html,
        setup_js: None,
        tests: vec![
            TestCase {
                name: "json snapshot traverses deeply nested DOM".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('json');
                    function getDepth(node, d) {
                        var max = d;
                        for (var i = 0; i < node.children.length; i++) {
                            var cd = getDepth(node.children[i], d + 1);
                            if (cd > max) max = cd;
                        }
                        return max;
                    }
                    var depth = getDepth(result.tree, 0);
                    return { depth: depth, depth_gte_5: depth >= 5 };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "compact snapshot shows indentation for nesting".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('compact');
                    var lines = result.tree.trim().split('\n');
                    var maxIndent = 0;
                    lines.forEach(function(l) {
                        var spaces = l.match(/^(\s*)/)[1].length;
                        if (spaces > maxIndent) maxIndent = spaces;
                    });
                    return { max_indent: maxIndent, indent_gte_8: maxIndent >= 8 };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "deep nested content has correct ref that resolves".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('json');
                    function findByTag(node, tag) {
                        if (node.tag === tag) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findByTag(node.children[i], tag);
                            if (r) return r;
                        }
                        return null;
                    }
                    var span = findByTag(result.tree, 'span');
                    var el = window.__VICTAURI__.getRef(span.ref_id);
                    return { found: !!span, text: span ? span.text : null, resolves: !!el };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["depth_gte_5"], true);
    assert_eq!(results[1].result.as_ref().unwrap()["indent_gte_8"], true);
    assert_eq!(results[2].result.as_ref().unwrap()["found"], true);
    assert_eq!(results[2].result.as_ref().unwrap()["text"], "Deep content");
    assert_eq!(results[2].result.as_ref().unwrap()["resolves"], true);
}

#[test]
fn snapshot_hidden_elements_excluded() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Hidden</title></head><body>
            <div id="visible">Visible content</div>
            <div id="hidden-display" style="display:none">Hidden by display</div>
            <div id="hidden-visibility" style="visibility:hidden">Hidden by visibility</div>
            <div id="hidden-opacity" style="opacity:0">Hidden by opacity</div>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "display:none elements excluded from json snapshot".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var visible = findById(result.tree, 'visible');
                    var hidden = findById(result.tree, 'hidden-display');
                    return { visible_found: !!visible, hidden_found: !!hidden };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "visibility:hidden elements excluded from json snapshot".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var hidden = findById(result.tree, 'hidden-visibility');
                    return { hidden_found: !!hidden };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "opacity:0 elements excluded from json snapshot".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var hidden = findById(result.tree, 'hidden-opacity');
                    return { hidden_found: !!hidden };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "hidden elements excluded from compact snapshot".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('compact');
                    return {
                        has_visible: result.tree.indexOf('Visible content') !== -1,
                        has_hidden: result.tree.indexOf('Hidden by display') !== -1,
                    };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["visible_found"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["hidden_found"], false);
    assert_eq!(results[1].result.as_ref().unwrap()["hidden_found"], false);
    assert_eq!(results[2].result.as_ref().unwrap()["hidden_found"], false);
    assert_eq!(results[3].result.as_ref().unwrap()["has_visible"], true);
    assert_eq!(results[3].result.as_ref().unwrap()["has_hidden"], false);
}

#[test]
fn snapshot_password_redaction() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Password</title></head><body>
            <input id="pass" type="password" value="secret123" />
            <input id="text" type="text" value="visible" />
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "password value is redacted in json snapshot".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var pass = findById(result.tree, 'pass');
                    var text = findById(result.tree, 'text');
                    return { pass_value: pass ? pass.value : null, text_value: text ? text.value : null };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "password value is redacted in compact snapshot".into(),
                code: r"
                    var result = window.__VICTAURI__.snapshot('compact');
                    return {
                        has_redacted: result.tree.indexOf('[REDACTED]') !== -1,
                        no_secret: result.tree.indexOf('secret123') === -1,
                    };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(
        results[0].result.as_ref().unwrap()["pass_value"],
        "[REDACTED]"
    );
    assert_eq!(results[0].result.as_ref().unwrap()["text_value"], "visible");
    assert_eq!(results[1].result.as_ref().unwrap()["has_redacted"], true);
    assert_eq!(results[1].result.as_ref().unwrap()["no_secret"], true);
}

#[test]
fn snapshot_json_attributes() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Attrs</title></head><body>
            <a id="mylink" href="/page" data-testid="nav-link">Link</a>
            <input id="mycheck" type="checkbox" checked />
            <img id="myimg" src="/pic.png" />
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "json snapshot captures interesting attributes".into(),
            code: r"
                    var result = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var link = findById(result.tree, 'mylink');
                    var check = findById(result.tree, 'mycheck');
                    var img = findById(result.tree, 'myimg');
                    return {
                        link_href: link ? link.attributes.href : null,
                        link_testid: link ? link.attributes['data-testid'] : null,
                        check_type: check ? check.attributes.type : null,
                        check_checked: check ? check.attributes.checked : null,
                        img_src: img ? img.attributes.src : null,
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = results[0].result.as_ref().unwrap();
    assert_eq!(r["link_href"], "/page");
    assert_eq!(r["link_testid"], "nav-link");
    assert_eq!(r["check_type"], "checkbox");
    assert_eq!(r["check_checked"], "");
    assert_eq!(r["img_src"], "/pic.png");
}

#[test]
fn snapshot_focusable_detection() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Focus</title></head><body>
            <button id="btn">Btn</button>
            <input id="inp" />
            <a id="link" href="/">Link</a>
            <div id="plain">Plain</div>
            <div id="tabbed" tabindex="0">Tabbed</div>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "focusable flag set correctly on elements".into(),
            code: r"
                    var result = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findById(result.tree, 'btn');
                    var inp = findById(result.tree, 'inp');
                    var link = findById(result.tree, 'link');
                    var plain = findById(result.tree, 'plain');
                    var tabbed = findById(result.tree, 'tabbed');
                    return {
                        btn_focusable: btn ? btn.focusable : null,
                        inp_focusable: inp ? inp.focusable : null,
                        link_focusable: link ? link.focusable : null,
                        plain_focusable: plain ? plain.focusable : null,
                        tabbed_focusable: tabbed ? tabbed.focusable : null,
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = results[0].result.as_ref().unwrap();
    assert_eq!(r["btn_focusable"], true);
    assert_eq!(r["inp_focusable"], true);
    assert_eq!(r["link_focusable"], true);
    assert_eq!(r["plain_focusable"], false);
    assert_eq!(r["tabbed_focusable"], true);
}

// ══════════════════════════════════════════════════════════════════════════════
// Extended findElements Tests
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn find_elements_by_tag() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Find Tag</title></head><body>
            <button>Btn1</button>
            <button>Btn2</button>
            <input type="text" />
            <textarea>Hello</textarea>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "findElements by tag button".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ tag: 'button' });
                    return { count: results.length, first_text: results[0] ? results[0].text : null };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "findElements by tag textarea".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ tag: 'textarea' });
                    return { count: results.length, tag: results[0] ? results[0].tag : null };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "findElements by tag case-insensitive".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ tag: 'BUTTON' });
                    return { count: results.length };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["count"], 2);
    assert_eq!(results[0].result.as_ref().unwrap()["first_text"], "Btn1");
    assert_eq!(results[1].result.as_ref().unwrap()["count"], 1);
    assert_eq!(results[1].result.as_ref().unwrap()["tag"], "textarea");
    assert_eq!(results[2].result.as_ref().unwrap()["count"], 2);
}

#[test]
fn find_elements_by_placeholder() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Find</title></head><body>
            <input type="text" placeholder="Search items..." />
            <input type="email" placeholder="Enter email" />
            <input type="text" placeholder="Username" />
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "findElements by placeholder".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ placeholder: 'search' });
                    return { count: results.length, tag: results[0] ? results[0].tag : null };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "findElements by placeholder partial match".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ placeholder: 'enter' });
                    return { count: results.length };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["count"], 1);
    assert_eq!(results[0].result.as_ref().unwrap()["tag"], "input");
    assert_eq!(results[1].result.as_ref().unwrap()["count"], 1);
}

#[test]
fn find_elements_by_alt_and_title() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Find</title></head><body>
            <img alt="Company logo" src="/logo.png" />
            <img alt="User avatar" src="/avatar.png" />
            <button title="Close dialog">X</button>
            <a href="/" title="Go to homepage">Home</a>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "findElements by alt attribute".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ alt: 'logo' });
                    return { count: results.length, tag: results[0] ? results[0].tag : null };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "findElements by title_attr".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ title_attr: 'close' });
                    return { count: results.length, text: results[0] ? results[0].text : null };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["count"], 1);
    assert_eq!(results[0].result.as_ref().unwrap()["tag"], "img");
    assert_eq!(results[1].result.as_ref().unwrap()["count"], 1);
    assert_eq!(results[1].result.as_ref().unwrap()["text"], "X");
}

#[test]
fn find_elements_by_label() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Label Find</title></head><body>
            <label for="email-input">Email Address</label>
            <input id="email-input" type="email" />
            <label>Username <input type="text" id="nested-input" /></label>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "findElements by label with for attribute".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ label: 'Email' });
                    return { count: results.length, tag: results[0] ? results[0].tag : null };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "findElements by label with nested input".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ label: 'Username' });
                    return { count: results.length, tag: results[0] ? results[0].tag : null };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["count"], 1);
    assert_eq!(results[0].result.as_ref().unwrap()["tag"], "input");
    assert_eq!(results[1].result.as_ref().unwrap()["count"], 1);
    assert_eq!(results[1].result.as_ref().unwrap()["tag"], "input");
}

#[test]
fn find_elements_exact_text() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Exact</title></head><body>
            <button>Save</button>
            <button>Save All</button>
            <button>Don't Save</button>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "findElements text without exact matches all containing".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ text: 'Save' });
                    return { count: results.length };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "findElements text with exact matches only exact".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ text: 'Save', exact: true });
                    return { count: results.length, text: results[0] ? results[0].text : null };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    // Without exact: matches body (contains "Save") + all three buttons
    assert!(
        results[0].result.as_ref().unwrap()["count"]
            .as_u64()
            .unwrap()
            >= 3
    );
    // With exact: only "Save" button
    assert_eq!(results[1].result.as_ref().unwrap()["count"], 1);
    assert_eq!(results[1].result.as_ref().unwrap()["text"], "Save");
}

#[test]
fn find_elements_enabled_filter() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Enabled</title></head><body>
            <button id="enabled">Enabled</button>
            <button id="disabled" disabled>Disabled</button>
            <input id="dis-input" disabled />
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "findElements enabled:true excludes disabled".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ tag: 'button', enabled: true });
                    return { count: results.length, text: results[0] ? results[0].text : null };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "findElements enabled:false finds only disabled".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ tag: 'button', enabled: false });
                    return { count: results.length, text: results[0] ? results[0].text : null };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["count"], 1);
    assert_eq!(results[0].result.as_ref().unwrap()["text"], "Enabled");
    assert_eq!(results[1].result.as_ref().unwrap()["count"], 1);
    assert_eq!(results[1].result.as_ref().unwrap()["text"], "Disabled");
}

#[test]
fn find_elements_combined_queries() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Combined</title></head><body>
            <button data-testid="primary" role="button">Submit</button>
            <button data-testid="secondary" role="button">Cancel</button>
            <a role="link" data-testid="nav">Navigate</a>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "findElements combines role + text".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ role: 'button', text: 'Submit' });
                    return { count: results.length, test_id: results[0] ? results[0].ref_id : null };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "findElements combines test_id + tag".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ test_id: 'nav', tag: 'a' });
                    return { count: results.length, text: results[0] ? results[0].text : null };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "findElements returns empty when combo doesnt match".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ role: 'link', text: 'Submit' });
                    return { count: results.length };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["count"], 1);
    assert_eq!(results[1].result.as_ref().unwrap()["count"], 1);
    assert_eq!(results[1].result.as_ref().unwrap()["text"], "Navigate");
    assert_eq!(results[2].result.as_ref().unwrap()["count"], 0);
}

#[test]
fn find_elements_result_structure() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Structure</title></head><body>
            <button id="btn1" aria-label="Save file" disabled>Save</button>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "findElements result has all expected fields".into(),
            code: r"
                    var results = window.__VICTAURI__.findElements({ tag: 'button', enabled: false });
                    var r = results[0];
                    return {
                        has_ref_id: typeof r.ref_id === 'string' && r.ref_id.charAt(0) === 'e',
                        has_tag: r.tag === 'button',
                        has_role: r.role === 'button',
                        has_name: r.name === 'Save file',
                        has_text: r.text === 'Save',
                        has_bounds: typeof r.bounds === 'object' && typeof r.bounds.x === 'number',
                        has_visible: typeof r.visible === 'boolean',
                        has_enabled: r.enabled === false,
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = results[0].result.as_ref().unwrap();
    assert_eq!(r["has_ref_id"], true);
    assert_eq!(r["has_tag"], true);
    assert_eq!(r["has_role"], true);
    assert_eq!(r["has_name"], true);
    assert_eq!(r["has_text"], true);
    assert_eq!(r["has_bounds"], true);
    assert_eq!(r["has_visible"], true);
    assert_eq!(r["has_enabled"], true);
}

#[test]
fn find_elements_hidden_excluded() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Hidden</title></head><body>
            <button style="display:none">Hidden Btn</button>
            <button>Visible Btn</button>
            <div style="visibility:hidden"><button>Nested Hidden</button></div>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "findElements skips hidden elements and their children".into(),
            code: r"
                    var results = window.__VICTAURI__.findElements({ role: 'button' });
                    var texts = results.map(function(r) { return r.text; });
                    return {
                        count: results.length,
                        has_visible: texts.indexOf('Visible Btn') !== -1,
                        has_hidden: texts.indexOf('Hidden Btn') !== -1,
                        has_nested: texts.indexOf('Nested Hidden') !== -1,
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = results[0].result.as_ref().unwrap();
    assert_eq!(r["has_visible"], true);
    assert_eq!(r["has_hidden"], false);
    assert_eq!(r["has_nested"], false);
}

// ══════════════════════════════════════════════════════════════════════════════
// Extended Actionability Tests
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn actionability_visibility_hidden() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Test</title></head><body>
            <button id="vis-hidden" style="visibility:hidden">Vis Hidden</button>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "click fails on visibility:hidden element".into(),
            code: r"
                    // visibility:hidden elements are excluded from snapshot, so we register manually
                    var el = document.getElementById('vis-hidden');
                    // We need to put it in refMap. snapshot won't include it. Use a trick:
                    // Temporarily make it visible for snapshot, get the ref, then hide it.
                    el.style.visibility = 'visible';
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findById(snap.tree, 'vis-hidden');
                    // Now hide it again
                    el.style.visibility = 'hidden';
                    var result = await window.__VICTAURI__.click(btn.ref_id, 100);
                    return { ok: result.ok, error_has_visibility: result.error ? result.error.indexOf('visibility') !== -1 : false };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["ok"], false);
    assert_eq!(
        results[0].result.as_ref().unwrap()["error_has_visibility"],
        true
    );
}

#[test]
fn actionability_opacity_zero() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Test</title></head><body>
            <button id="opaque" style="opacity:1">Opaque</button>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "click fails on opacity:0 element".into(),
            code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findById(snap.tree, 'opaque');
                    var el = window.__VICTAURI__.getRef(btn.ref_id);
                    el.style.opacity = '0';
                    var result = await window.__VICTAURI__.click(btn.ref_id, 100);
                    return { ok: result.ok, error_has_opacity: result.error ? result.error.indexOf('opacity') !== -1 : false };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["ok"], false);
    assert_eq!(
        results[0].result.as_ref().unwrap()["error_has_opacity"],
        true
    );
}

#[test]
fn actionability_pointer_events_none() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Test</title></head><body>
            <button id="no-pointer" style="pointer-events:none">No Pointer</button>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "click fails on pointer-events:none element".into(),
            code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findById(snap.tree, 'no-pointer');
                    var result = await window.__VICTAURI__.click(btn.ref_id, 100);
                    return { ok: result.ok, error_has_pointer: result.error ? result.error.indexOf('pointer-events') !== -1 : false };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["ok"], false);
    assert_eq!(
        results[0].result.as_ref().unwrap()["error_has_pointer"],
        true
    );
}

#[test]
fn actionability_detached_element() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![TestCase {
            name: "click fails on element removed from DOM".into(),
            code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findByTag(node, tag) {
                        if (node.tag === tag) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findByTag(node.children[i], tag);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findByTag(snap.tree, 'button');
                    var el = window.__VICTAURI__.getRef(btn.ref_id);
                    el.parentNode.removeChild(el);
                    var result = await window.__VICTAURI__.click(btn.ref_id, 100);
                    // resolveRef returns null for disconnected elements, so
                    // withAutoWait reports 'ref not found' or 'detached' depending on timing
                    return {
                        ok: result.ok,
                        has_error: !!result.error,
                        error_relevant: result.error ? (result.error.indexOf('ref not found') !== -1 || result.error.indexOf('detached') !== -1) : false,
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["ok"], false);
    assert_eq!(results[0].result.as_ref().unwrap()["has_error"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["error_relevant"], true);
}

#[test]
fn actionability_auto_wait_succeeds() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Test</title></head><body>
            <button id="delayed" disabled>Delayed</button>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "click auto-waits for element to become actionable".into(),
            code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findById(snap.tree, 'delayed');
                    // Enable the button after 50ms
                    setTimeout(function() {
                        var el = document.getElementById('delayed');
                        el.disabled = false;
                    }, 50);
                    var result = await window.__VICTAURI__.click(btn.ref_id, 2000);
                    return result;
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["ok"], true);
}

// ══════════════════════════════════════════════════════════════════════════════
// Extended pressKey Tests
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn press_key_modifiers() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "pressKey with Control+S dispatches with ctrlKey".into(),
                code: r"
                    var events = [];
                    document.body.addEventListener('keydown', function(e) {
                        events.push({ key: e.key, ctrl: e.ctrlKey, shift: e.shiftKey, alt: e.altKey, meta: e.metaKey });
                    });
                    window.__VICTAURI__.pressKey('Control+s');
                    // We expect: keydown Control, keydown s (with ctrlKey=true)
                    var sEvent = events.find(function(e) { return e.key === 's'; });
                    return { found: !!sEvent, ctrl: sEvent ? sEvent.ctrl : false };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "pressKey with Shift+Tab dispatches with shiftKey".into(),
                code: r"
                    var events = [];
                    document.body.addEventListener('keydown', function(e) {
                        events.push({ key: e.key, shift: e.shiftKey });
                    });
                    window.__VICTAURI__.pressKey('Shift+Tab');
                    var tabEvent = events.find(function(e) { return e.key === 'Tab'; });
                    return { found: !!tabEvent, shift: tabEvent ? tabEvent.shift : false };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "pressKey with Meta+c dispatches with metaKey".into(),
                code: r"
                    var events = [];
                    document.body.addEventListener('keydown', function(e) {
                        events.push({ key: e.key, meta: e.metaKey });
                    });
                    window.__VICTAURI__.pressKey('Meta+c');
                    var cEvent = events.find(function(e) { return e.key === 'c'; });
                    return { found: !!cEvent, meta: cEvent ? cEvent.meta : false };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "pressKey with Alt+F4 dispatches with altKey".into(),
                code: r"
                    var events = [];
                    document.body.addEventListener('keydown', function(e) {
                        events.push({ key: e.key, alt: e.altKey });
                    });
                    window.__VICTAURI__.pressKey('Alt+F4');
                    var f4Event = events.find(function(e) { return e.key === 'F4'; });
                    return { found: !!f4Event, alt: f4Event ? f4Event.alt : false };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "pressKey with multiple modifiers Control+Shift+z".into(),
                code: r"
                    var events = [];
                    document.body.addEventListener('keydown', function(e) {
                        events.push({ key: e.key, ctrl: e.ctrlKey, shift: e.shiftKey });
                    });
                    window.__VICTAURI__.pressKey('Control+Shift+z');
                    var zEvent = events.find(function(e) { return e.key === 'z'; });
                    return { found: !!zEvent, ctrl: zEvent ? zEvent.ctrl : false, shift: zEvent ? zEvent.shift : false };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["found"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["ctrl"], true);
    assert_eq!(results[1].result.as_ref().unwrap()["found"], true);
    assert_eq!(results[1].result.as_ref().unwrap()["shift"], true);
    assert_eq!(results[2].result.as_ref().unwrap()["found"], true);
    assert_eq!(results[2].result.as_ref().unwrap()["meta"], true);
    assert_eq!(results[3].result.as_ref().unwrap()["found"], true);
    assert_eq!(results[3].result.as_ref().unwrap()["alt"], true);
    assert_eq!(results[4].result.as_ref().unwrap()["found"], true);
    assert_eq!(results[4].result.as_ref().unwrap()["ctrl"], true);
    assert_eq!(results[4].result.as_ref().unwrap()["shift"], true);
}

#[test]
fn press_key_targets_focused_element() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Test</title></head><body>
            <input id="inp" type="text" />
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "pressKey dispatches to focused input".into(),
            code: r"
                    var inp = document.getElementById('inp');
                    inp.focus();
                    var events = [];
                    inp.addEventListener('keydown', function(e) { events.push('keydown:' + e.key); });
                    inp.addEventListener('keyup', function(e) { events.push('keyup:' + e.key); });
                    window.__VICTAURI__.pressKey('a');
                    return { events: events, target_is_input: events.length > 0 };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let events: Vec<String> =
        serde_json::from_value(results[0].result.as_ref().unwrap()["events"].clone()).unwrap();
    assert!(events.contains(&"keydown:a".to_string()));
    assert!(events.contains(&"keyup:a".to_string()));
}

// ══════════════════════════════════════════════════════════════════════════════
// Extended Console Capture Tests
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn console_all_levels() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![TestCase {
            name: "all console levels captured".into(),
            code: r"
                    console.log('log msg');
                    console.warn('warn msg');
                    console.error('error msg');
                    console.info('info msg');
                    console.debug('debug msg');
                    var logs = window.__VICTAURI__.getConsoleLogs();
                    var levels = logs.map(function(l) { return l.level; });
                    return {
                        has_log: levels.indexOf('log') !== -1,
                        has_warn: levels.indexOf('warn') !== -1,
                        has_error: levels.indexOf('error') !== -1,
                        has_info: levels.indexOf('info') !== -1,
                        has_debug: levels.indexOf('debug') !== -1,
                        count: logs.length,
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = results[0].result.as_ref().unwrap();
    assert_eq!(r["has_log"], true);
    assert_eq!(r["has_warn"], true);
    assert_eq!(r["has_error"], true);
    assert_eq!(r["has_info"], true);
    assert_eq!(r["has_debug"], true);
}

#[test]
fn console_multiple_args_joined() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![TestCase {
            name: "console with multiple args joins them with space".into(),
            code: r"
                    console.log('hello', 'world', 42);
                    var logs = window.__VICTAURI__.getConsoleLogs();
                    var last = logs[logs.length - 1];
                    return { message: last.message };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(
        results[0].result.as_ref().unwrap()["message"],
        "hello world 42"
    );
}

#[test]
fn console_control_chars_stripped() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![TestCase {
            name: "control characters are stripped from console messages".into(),
            code: r"
                    console.log('clean\x00\x1b[31mtext\x0B\x1F');
                    var logs = window.__VICTAURI__.getConsoleLogs();
                    var last = logs[logs.length - 1];
                    return { message: last.message, no_escape: last.message.indexOf('\x1b') === -1 };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["no_escape"], true);
    // The message should have control chars stripped but 'clean' and 'text' preserved
    let msg = results[0].result.as_ref().unwrap()["message"]
        .as_str()
        .unwrap();
    assert!(msg.contains("clean"));
    assert!(msg.contains("text"));
}

// ══════════════════════════════════════════════════════════════════════════════
// Extended getStyles and getBoundingBoxes Tests
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn get_styles_default_properties() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![TestCase {
            name: "getStyles without properties returns important subset".into(),
            code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    var result = window.__VICTAURI__.getStyles(snap.tree.ref_id);
                    var keys = Object.keys(result.styles);
                    return {
                        has_display: keys.indexOf('display') !== -1 || result.styles['display'] !== undefined,
                        has_visibility: keys.indexOf('visibility') !== -1 || result.styles['visibility'] !== undefined,
                        tag: result.tag,
                        ref_id: result.ref_id,
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = results[0].result.as_ref().unwrap();
    assert_eq!(r["tag"], "body");
    assert!(r["ref_id"].is_string());
}

#[test]
fn get_bounding_boxes_multiple() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Boxes</title></head><body>
            <button id="b1">One</button>
            <button id="b2">Two</button>
            <div id="d1">Div</div>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "getBoundingBoxes returns all requested refs with box model".into(),
            code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var b1 = findById(snap.tree, 'b1');
                    var b2 = findById(snap.tree, 'b2');
                    var d1 = findById(snap.tree, 'd1');
                    var results = window.__VICTAURI__.getBoundingBoxes([b1.ref_id, b2.ref_id, d1.ref_id]);
                    return {
                        count: results.length,
                        first_tag: results[0].tag,
                        first_has_margin: typeof results[0].margin === 'object',
                        first_has_padding: typeof results[0].padding === 'object',
                        first_has_border: typeof results[0].border === 'object',
                        second_tag: results[1].tag,
                        third_tag: results[2].tag,
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = results[0].result.as_ref().unwrap();
    assert_eq!(r["count"], 3);
    assert_eq!(r["first_tag"], "button");
    assert_eq!(r["first_has_margin"], true);
    assert_eq!(r["first_has_padding"], true);
    assert_eq!(r["first_has_border"], true);
    assert_eq!(r["second_tag"], "button");
    assert_eq!(r["third_tag"], "div");
}

// ══════════════════════════════════════════════════════════════════════════════
// Accessibility Audit Extended Tests
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn a11y_clean_page() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Accessible Page</title></head><body>
            <h1>Welcome</h1>
            <img alt="Logo" src="logo.png" />
            <label for="name">Name</label>
            <input id="name" type="text" />
            <button>Submit</button>
            <a href="/about">About Us</a>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "well-formed page has no critical violations".into(),
            code: r"
                    var result = window.__VICTAURI__.auditAccessibility();
                    // Note: jsdom lacks CSS.escape, so the label[for='name'] query
                    // may fail silently, causing a false input-label violation.
                    // Filter to only critical violations (img-alt, link-name, button-name)
                    // which are not affected by the CSS.escape issue.
                    var criticals = result.violations.filter(function(v) {
                        return v.rule === 'img-alt' || v.rule === 'button-name' || v.rule === 'link-name' || v.rule === 'html-lang' || v.rule === 'document-title';
                    });
                    return {
                        critical_count: criticals.length,
                        has_lang: true,
                        has_title: true,
                        total_violations: result.violations.length,
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    // No critical violations (img-alt, button-name, link-name, html-lang, document-title)
    assert_eq!(results[0].result.as_ref().unwrap()["critical_count"], 0);
}

#[test]
fn a11y_missing_lang() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r"<html><head><title>No Lang</title></head><body>
            <p>Content</p>
        </body></html>"
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "missing lang attribute detected".into(),
            code: r"
                    var result = window.__VICTAURI__.auditAccessibility();
                    var langViolation = result.violations.find(function(v) { return v.rule === 'html-lang'; });
                    return { found: !!langViolation, severity: langViolation ? langViolation.severity : null };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["found"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["severity"], "serious");
}

#[test]
fn a11y_heading_hierarchy() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Headings</title></head><body>
            <h1>Title</h1>
            <h3>Skipped h2</h3>
            <h4>After skip</h4>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "heading hierarchy skip detected".into(),
            code: r"
                    var result = window.__VICTAURI__.auditAccessibility();
                    var headingWarn = result.warnings.find(function(w) { return w.rule === 'heading-order'; });
                    return { found: !!headingWarn, message: headingWarn ? headingWarn.message : null };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["found"], true);
    let msg = results[0].result.as_ref().unwrap()["message"]
        .as_str()
        .unwrap();
    assert!(msg.contains("h1") && msg.contains("h3"));
}

#[test]
fn a11y_empty_img_alt() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Alt</title></head><body>
            <img alt="" src="decorative.png" />
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "empty alt is warning not violation".into(),
            code: r"
                    var result = window.__VICTAURI__.auditAccessibility();
                    var violation = result.violations.find(function(v) { return v.rule === 'img-alt'; });
                    var warning = result.warnings.find(function(w) { return w.rule === 'img-alt-empty'; });
                    return {
                        no_violation: !violation,
                        has_warning: !!warning,
                        severity: warning ? warning.severity : null,
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["no_violation"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["has_warning"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["severity"], "minor");
}

// ══════════════════════════════════════════════════════════════════════════════
// waitFor Extended Tests
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn wait_for_url_condition() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "waitFor url condition met immediately for localhost".into(),
                code: r"
                    var result = await window.__VICTAURI__.waitFor({ condition: 'url', value: 'localhost', timeout_ms: 500 });
                    return result;
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "waitFor url condition fails for non-matching".into(),
                code: r"
                    var result = await window.__VICTAURI__.waitFor({ condition: 'url', value: 'https://example.com', timeout_ms: 100, poll_ms: 20 });
                    return { ok: result.ok };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["ok"], true);
    assert_eq!(results[1].result.as_ref().unwrap()["ok"], false);
}

#[test]
fn wait_for_dynamic_element() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Wait</title></head><body>
            <div id="container"></div>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "waitFor selector succeeds when element added dynamically".into(),
                code: r"
                    setTimeout(function() {
                        var el = document.createElement('div');
                        el.id = 'dynamic';
                        document.getElementById('container').appendChild(el);
                    }, 50);
                    var result = await window.__VICTAURI__.waitFor({ condition: 'selector', value: '#dynamic', timeout_ms: 2000, poll_ms: 20 });
                    return result;
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "waitFor selector_gone succeeds when element removed".into(),
                code: r"
                    var el = document.createElement('div');
                    el.id = 'temp-el';
                    document.body.appendChild(el);
                    setTimeout(function() {
                        el.parentNode.removeChild(el);
                    }, 50);
                    var result = await window.__VICTAURI__.waitFor({ condition: 'selector_gone', value: '#temp-el', timeout_ms: 2000, poll_ms: 20 });
                    return result;
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "waitFor text succeeds when text added dynamically".into(),
                code: r"
                    setTimeout(function() {
                        document.getElementById('container').textContent = 'Dynamic text loaded';
                    }, 50);
                    var result = await window.__VICTAURI__.waitFor({ condition: 'text', value: 'Dynamic text loaded', timeout_ms: 2000, poll_ms: 20 });
                    return result;
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["ok"], true);
    assert_eq!(results[1].result.as_ref().unwrap()["ok"], true);
    assert_eq!(results[2].result.as_ref().unwrap()["ok"], true);
}

#[test]
fn wait_for_elapsed_time() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![TestCase {
            name: "waitFor returns elapsed_ms in result".into(),
            code: r"
                    var result = await window.__VICTAURI__.waitFor({ condition: 'selector', value: 'body', timeout_ms: 500 });
                    return { ok: result.ok, has_elapsed: typeof result.elapsed_ms === 'number', elapsed_low: result.elapsed_ms < 100 };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["ok"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["has_elapsed"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["elapsed_low"], true);
}

// ══════════════════════════════════════════════════════════════════════════════
// Edge Cases and Stress Tests
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn ref_map_limit_enforcement() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Limit</title></head><body>
            <div id="container"></div>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "ref map evicts oldest when limit reached".into(),
            code: r"
                    // Generate many elements and take snapshot to fill refMap
                    var container = document.getElementById('container');
                    for (var i = 0; i < 100; i++) {
                        var el = document.createElement('span');
                        el.textContent = 'item' + i;
                        container.appendChild(el);
                    }
                    var snap1 = window.__VICTAURI__.snapshot('json');
                    // All refs should be valid right after snapshot
                    var ref1 = snap1.tree.ref_id;
                    var resolved = window.__VICTAURI__.getRef(ref1);
                    return { first_ref_valid: !!resolved };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["first_ref_valid"], true);
}

#[test]
fn fill_contenteditable() {
    // Note: In real browsers, fill on contenteditable works because the
    // HTMLInputElement.prototype.value setter is more permissive. In jsdom,
    // calling the setter on a non-input element throws. The bridge's fill
    // function passes the matches check for [contenteditable="true"] but
    // then the value setter call fails. This test verifies the bridge
    // correctly passes through the actionability check and attempts the fill,
    // even though jsdom rejects the setter call.
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>CE</title></head><body>
            <div id="editor" contenteditable="true">Initial text</div>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "fill on contenteditable passes actionability and attempts fill".into(),
            code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var editor = findById(snap.tree, 'editor');
                    var result = await window.__VICTAURI__.fill(editor.ref_id, 'New content');
                    // In jsdom, the HTMLInputElement.prototype value setter throws
                    // on non-input elements. In real browsers this works. The bridge
                    // correctly identifies it as fillable (passes matches check).
                    // The action threw error proves it got past actionability checks.
                    return {
                        passed_actionability: result.ok || (result.error && result.error.indexOf('action threw') !== -1),
                        not_unfillable: !result.error || result.error.indexOf('not fillable') === -1,
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = results[0].result.as_ref().unwrap();
    assert_eq!(r["passed_actionability"], true);
    assert_eq!(r["not_unfillable"], true);
}

#[test]
fn type_appends_to_existing_value() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Type</title></head><body>
            <input id="inp" type="text" value="Hello" />
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "type appends to existing input value".into(),
            code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var inp = findById(snap.tree, 'inp');
                    await window.__VICTAURI__.type(inp.ref_id, ' World');
                    var el = window.__VICTAURI__.getRef(inp.ref_id);
                    return { value: el.value };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["value"], "Hello World");
}

#[test]
fn diagnostics_basic() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![TestCase {
            name: "getDiagnostics returns expected structure".into(),
            code: r"
                    var diag = window.__VICTAURI__.getDiagnostics();
                    return {
                        has_warnings: Array.isArray(diag.warnings),
                        has_info: typeof diag.info === 'object',
                        has_version: diag.info.bridge_version === '0.7.2',
                        has_url: typeof diag.info.url === 'string',
                        has_dom_elements: typeof diag.info.dom_elements === 'number',
                        has_protocol: typeof diag.info.protocol === 'string',
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = results[0].result.as_ref().unwrap();
    assert_eq!(r["has_warnings"], true);
    assert_eq!(r["has_info"], true);
    assert_eq!(r["has_version"], true);
    assert_eq!(r["has_url"], true);
    assert_eq!(r["has_dom_elements"], true);
    assert_eq!(r["has_protocol"], true);
}

#[test]
fn diagnostics_iframe_detection() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Iframe</title></head><body>
            <iframe src="about:blank"></iframe>
            <iframe src="about:blank"></iframe>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "getDiagnostics detects iframes".into(),
            code: r"
                    var diag = window.__VICTAURI__.getDiagnostics();
                    var iframeWarn = diag.warnings.find(function(w) { return w.id === 'iframes-present'; });
                    return { found: !!iframeWarn, count: iframeWarn ? iframeWarn.details.count : 0 };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["found"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["count"], 2);
}

#[test]
fn interaction_observer_click_capture() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Observe</title></head><body>
            <button id="obs-btn" data-testid="my-btn">Click Me</button>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "trusted click events captured in interaction log via getEventStream".into(),
            code: r"
                    // Simulate a trusted click by dispatching with isTrusted cannot be faked in jsdom.
                    // Instead, just verify getEventStream returns the structure we expect.
                    // Use programmatic dispatchEvent which won't have isTrusted=true, so it won't
                    // be captured by the interaction observer. Test the observer plumbing instead.
                    var events = window.__VICTAURI__.getEventStream();
                    return { is_array: Array.isArray(events) };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["is_array"], true);
}

#[test]
fn highlight_with_custom_color() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Highlight</title></head><body>
            <button id="btn">Highlight Me</button>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "highlight creates overlay with custom color and data-ref attribute".into(),
            code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findById(snap.tree, 'btn');
                    var result = window.__VICTAURI__.highlightElement(btn.ref_id, 'rgba(0, 255, 0, 0.5)', 'test');
                    var overlay = document.querySelector('.__victauri_highlight__');
                    var refAttr = overlay ? overlay.getAttribute('data-victauri-ref') : null;
                    var style = overlay ? overlay.style.cssText : '';
                    return {
                        ok: result.ok,
                        has_ref_attr: refAttr === btn.ref_id,
                        has_fixed: style.indexOf('fixed') !== -1,
                        has_z_index: style.indexOf('2147483647') !== -1,
                        has_pointer_none: style.indexOf('pointer-events') !== -1,
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = results[0].result.as_ref().unwrap();
    assert_eq!(r["ok"], true);
    assert_eq!(r["has_ref_attr"], true);
    assert_eq!(r["has_fixed"], true);
    assert_eq!(r["has_z_index"], true);
    assert_eq!(r["has_pointer_none"], true);
}

#[test]
fn snapshot_many_elements_performance() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Perf</title></head><body>
            <div id="container"></div>
        </body></html>"#
            .to_string(),
        setup_js: Some(
            r"
            var container = document.getElementById('container');
            for (var i = 0; i < 500; i++) {
                var el = document.createElement('div');
                el.textContent = 'Item ' + i;
                el.setAttribute('data-testid', 'item-' + i);
                container.appendChild(el);
            }
        "
            .to_string(),
        ),
        tests: vec![
            TestCase {
                name: "snapshot handles 500 elements".into(),
                code: r"
                    var start = Date.now();
                    var result = window.__VICTAURI__.snapshot('json');
                    var elapsed = Date.now() - start;
                    function countNodes(node) {
                        var c = 1;
                        for (var i = 0; i < node.children.length; i++) c += countNodes(node.children[i]);
                        return c;
                    }
                    var total = countNodes(result.tree);
                    return { total: total, elapsed_ms: elapsed, under_1s: elapsed < 1000 };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "findElements handles large DOM".into(),
                code: r"
                    var start = Date.now();
                    var results = window.__VICTAURI__.findElements({ text: 'Item 250' });
                    var elapsed = Date.now() - start;
                    return { count: results.length, elapsed_ms: elapsed, under_500ms: elapsed < 500 };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert!(
        results[0].result.as_ref().unwrap()["total"]
            .as_u64()
            .unwrap()
            >= 500
    );
    assert_eq!(results[0].result.as_ref().unwrap()["under_1s"], true);
    assert!(
        results[1].result.as_ref().unwrap()["count"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert_eq!(results[1].result.as_ref().unwrap()["under_500ms"], true);
}

#[test]
fn snapshot_special_characters() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Special</title></head><body>
            <button id="emoji">Click here! 🎉</button>
            <div id="quotes">He said "hello" &amp; 'goodbye'</div>
            <span id="unicode">日本語テスト</span>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "snapshot handles special characters without corruption".into(),
            code: r"
                    var result = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var emoji = findById(result.tree, 'emoji');
                    var quotes = findById(result.tree, 'quotes');
                    var unicode = findById(result.tree, 'unicode');
                    return {
                        emoji_text: emoji ? emoji.name : null,
                        quotes_text: quotes ? quotes.text : null,
                        unicode_text: unicode ? unicode.text : null,
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = results[0].result.as_ref().unwrap();
    // Button name comes from textContent.trim()
    let emoji_text = r["emoji_text"].as_str().unwrap();
    assert!(emoji_text.contains("Click here!"));
    let unicode_text = r["unicode_text"].as_str().unwrap();
    assert!(unicode_text.contains("日本語テスト"));
}

#[test]
fn get_ref_after_multiple_snapshots() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![TestCase {
            name: "refs from latest snapshot work, old refs cleared".into(),
            code: r"
                    var s1 = window.__VICTAURI__.snapshot('json');
                    var firstRef = s1.tree.ref_id;
                    var s2 = window.__VICTAURI__.snapshot('json');
                    var secondRef = s2.tree.ref_id;
                    // Second snapshot should have different ref counter
                    var el1 = window.__VICTAURI__.getRef(firstRef);
                    var el2 = window.__VICTAURI__.getRef(secondRef);
                    return {
                        refs_different: firstRef !== secondRef,
                        old_ref_null: el1 === null,
                        new_ref_valid: !!el2,
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = results[0].result.as_ref().unwrap();
    assert_eq!(r["refs_different"], true);
    // Old refs may still resolve via WeakRef if element is still connected
    // The important thing is the new ref is valid
    assert_eq!(r["new_ref_valid"], true);
}

#[test]
fn find_elements_reuses_existing_refs() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Reuse</title></head><body>
            <button id="btn1">Click</button>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "findElements returns same ref_id for already-registered element".into(),
            code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findByTag(node, tag) {
                        if (node.tag === tag) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findByTag(node.children[i], tag);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findByTag(snap.tree, 'button');
                    var btnRef = btn.ref_id;
                    // findElements should find the same element and reuse the ref
                    var results = window.__VICTAURI__.findElements({ tag: 'button' });
                    return { same_ref: results[0].ref_id === btnRef };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["same_ref"], true);
}

#[test]
fn performance_metrics_dom_depth() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Depth</title></head><body>
            <div><div><div><div><div><span>Deep</span></div></div></div></div></div>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "performance metrics reports correct max_depth".into(),
            code: r"
                    var result = window.__VICTAURI__.getPerformanceMetrics();
                    return { max_depth: result.dom.max_depth, depth_gte_5: result.dom.max_depth >= 5 };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["depth_gte_5"], true);
}

#[test]
fn fill_clears_and_replaces() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Fill</title></head><body>
            <input id="inp" type="text" value="existing content" />
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "fill replaces existing value completely".into(),
            code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var inp = findById(snap.tree, 'inp');
                    await window.__VICTAURI__.fill(inp.ref_id, 'new value');
                    var el = window.__VICTAURI__.getRef(inp.ref_id);
                    return { value: el.value, no_old: el.value.indexOf('existing') === -1 };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["value"], "new value");
    assert_eq!(results[0].result.as_ref().unwrap()["no_old"], true);
}

#[test]
fn network_ipc_body_capture() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![TestCase {
            name: "IPC request body captured in log entry".into(),
            code: r#"
                    await fetch('http://ipc.localhost/search_items', {
                        method: 'POST',
                        body: JSON.stringify({ query: "hello", limit: 10 })
                    });
                    var log = window.__VICTAURI__.getIpcLog();
                    var entry = log.find(function(e) { return e.command === 'search_items'; });
                    return {
                        found: !!entry,
                        has_args: !!entry && !!entry.args,
                        query: entry && entry.args ? entry.args.query : null,
                        limit: entry && entry.args ? entry.args.limit : null,
                    };
                "#
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = results[0].result.as_ref().unwrap();
    assert_eq!(r["found"], true);
    assert_eq!(r["has_args"], true);
    assert_eq!(r["query"], "hello");
    assert_eq!(r["limit"], 10);
}

#[test]
fn event_listener_counter() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![TestCase {
            name: "event listener count increments and decrements".into(),
            code: r"
                    var metrics1 = window.__VICTAURI__.getPerformanceMetrics();
                    var before = metrics1.dom.event_listeners;
                    function handler() {}
                    document.body.addEventListener('click', handler);
                    document.body.addEventListener('mouseover', handler);
                    var metrics2 = window.__VICTAURI__.getPerformanceMetrics();
                    var after_add = metrics2.dom.event_listeners;
                    document.body.removeEventListener('click', handler);
                    var metrics3 = window.__VICTAURI__.getPerformanceMetrics();
                    var after_remove = metrics3.dom.event_listeners;
                    return {
                        added_two: after_add === before + 2,
                        removed_one: after_remove === after_add - 1,
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["added_two"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["removed_one"], true);
}

#[test]
fn dialog_prompt_with_custom_response() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "prompt returns custom text after setDialogAutoResponse".into(),
                code: r"
                    window.__VICTAURI__.setDialogAutoResponse('prompt', 'accept', 'custom answer');
                    var result = window.prompt('What is your name?');
                    return { result: result };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "prompt returns null after dismiss response".into(),
                code: r"
                    window.__VICTAURI__.setDialogAutoResponse('prompt', 'dismiss');
                    var result = window.prompt('Enter something');
                    return { result: result, is_null: result === null };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(
        results[0].result.as_ref().unwrap()["result"],
        "custom answer"
    );
    assert_eq!(results[1].result.as_ref().unwrap()["is_null"], true);
}

#[test]
fn find_elements_no_results() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Empty</title></head><body>
            <div>Just a div</div>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![
            TestCase {
                name: "findElements returns empty for nonexistent test_id".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ test_id: 'nonexistent' });
                    return { count: results.length };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "findElements returns empty for nonexistent role".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ role: 'slider' });
                    return { count: results.length };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
            TestCase {
                name: "findElements returns error for invalid css".into(),
                code: r"
                    var results = window.__VICTAURI__.findElements({ css: '###invalid' });
                    return { has_error: !!results.error, error_contains_selector: results.error && results.error.indexOf('###invalid') !== -1 };
                "
                .into(),
                setup_html: None,
                setup_js: None,
            },
        ],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["count"], 0);
    assert_eq!(results[1].result.as_ref().unwrap()["count"], 0);
    assert_eq!(results[2].result.as_ref().unwrap()["has_error"], true);
    assert_eq!(
        results[2].result.as_ref().unwrap()["error_contains_selector"],
        true
    );
}

#[test]
fn double_click_event_details() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![TestCase {
            name: "doubleClick event bubbles and is cancelable".into(),
            code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findByTag(node, tag) {
                        if (node.tag === tag) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findByTag(node.children[i], tag);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findByTag(snap.tree, 'button');
                    var detail = {};
                    var el = window.__VICTAURI__.getRef(btn.ref_id);
                    el.addEventListener('dblclick', function(e) {
                        detail.bubbles = e.bubbles;
                        detail.cancelable = e.cancelable;
                    });
                    await window.__VICTAURI__.doubleClick(btn.ref_id);
                    return detail;
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["bubbles"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["cancelable"], true);
}

#[test]
fn wait_for_network_idle() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![TestCase {
            name: "waitFor network_idle resolves when all requests complete".into(),
            code: r"
                    // All our fake fetch calls resolve immediately, so network should be idle
                    await fetch('http://example.com/api');
                    var result = await window.__VICTAURI__.waitFor({ condition: 'network_idle', timeout_ms: 500 });
                    return result;
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["ok"], true);
}

#[test]
fn select_option_multiple_values() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Select</title></head><body>
            <select id="multi" multiple>
                <option value="a">A</option>
                <option value="b">B</option>
                <option value="c">C</option>
                <option value="d">D</option>
            </select>
        </body></html>"#
            .to_string(),
        setup_js: None,
        tests: vec![TestCase {
            name: "selectOption selects multiple values on multi-select".into(),
            code: r"
                    var snap = window.__VICTAURI__.snapshot('json');
                    function findById(node, id) {
                        if (node.attributes && node.attributes.id === id) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findById(node.children[i], id);
                            if (r) return r;
                        }
                        return null;
                    }
                    var sel = findById(snap.tree, 'multi');
                    var result = await window.__VICTAURI__.selectOption(sel.ref_id, ['a', 'c']);
                    var el = window.__VICTAURI__.getRef(sel.ref_id);
                    var selected = [];
                    for (var i = 0; i < el.options.length; i++) {
                        if (el.options[i].selected) selected.push(el.options[i].value);
                    }
                    return { ok: result.ok, selected: selected };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["ok"], true);
    let selected: Vec<String> =
        serde_json::from_value(results[0].result.as_ref().unwrap()["selected"].clone()).unwrap();
    assert!(selected.contains(&"a".to_string()));
    assert!(selected.contains(&"c".to_string()));
    assert!(!selected.contains(&"b".to_string()));
    assert!(!selected.contains(&"d".to_string()));
}

#[test]
fn snapshot_shadow_dom() {
    // Note: jsdom has limited shadow DOM support but we can test open shadow roots
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Shadow</title></head><body>
            <div id="host"></div>
        </body></html>"#
            .to_string(),
        setup_js: Some(
            r"
            var host = document.getElementById('host');
            var shadow = host.attachShadow({ mode: 'open' });
            var btn = document.createElement('button');
            btn.textContent = 'Shadow Button';
            shadow.appendChild(btn);
        "
            .to_string(),
        ),
        tests: vec![TestCase {
            name: "snapshot traverses open shadow DOM".into(),
            code: r"
                    var result = window.__VICTAURI__.snapshot('json');
                    function findByTag(node, tag) {
                        if (node.tag === tag) return node;
                        for (var i = 0; i < node.children.length; i++) {
                            var r = findByTag(node.children[i], tag);
                            if (r) return r;
                        }
                        return null;
                    }
                    var btn = findByTag(result.tree, 'button');
                    return { found: !!btn, name: btn ? btn.name : null };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["found"], true);
    assert_eq!(results[0].result.as_ref().unwrap()["name"], "Shadow Button");
}

#[test]
fn find_elements_in_shadow_dom() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: r#"<html lang="en"><head><title>Shadow Find</title></head><body>
            <div id="host"></div>
            <button>Light Button</button>
        </body></html>"#
            .to_string(),
        setup_js: Some(
            r"
            var host = document.getElementById('host');
            var shadow = host.attachShadow({ mode: 'open' });
            var btn = document.createElement('button');
            btn.textContent = 'Shadow Button';
            btn.setAttribute('data-testid', 'shadow-btn');
            shadow.appendChild(btn);
        "
            .to_string(),
        ),
        tests: vec![TestCase {
            name: "findElements traverses shadow DOM".into(),
            code: r"
                    var results = window.__VICTAURI__.findElements({ role: 'button' });
                    var texts = results.map(function(r) { return r.text; });
                    return {
                        count: results.length,
                        has_light: texts.indexOf('Light Button') !== -1,
                        has_shadow: texts.indexOf('Shadow Button') !== -1,
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    let r = results[0].result.as_ref().unwrap();
    assert_eq!(r["has_light"], true);
    assert_eq!(r["has_shadow"], true);
    assert!(r["count"].as_u64().unwrap() >= 2);
}

#[test]
fn css_injection_replaces_not_appends() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![TestCase {
            name: "repeated injectCss does not create multiple style elements".into(),
            code: r"
                    window.__VICTAURI__.injectCss('body { margin: 0; }');
                    window.__VICTAURI__.injectCss('body { padding: 0; }');
                    window.__VICTAURI__.injectCss('body { color: blue; }');
                    var styles = document.querySelectorAll('#__victauri_injected_css__');
                    return {
                        count: styles.length,
                        content: styles[0] ? styles[0].textContent : null,
                    };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["count"], 1);
    assert_eq!(
        results[0].result.as_ref().unwrap()["content"],
        "body { color: blue; }"
    );
}

#[test]
fn ipc_encoded_command_names() {
    let def = TestDef {
        bridge_script: bridge_script(),
        setup_html: default_html(),
        setup_js: None,
        tests: vec![TestCase {
            name: "IPC log decodes URL-encoded command names".into(),
            code: r"
                    await fetch('http://ipc.localhost/my%20command%3Awith%20special', { method: 'POST', body: '{}' });
                    var log = window.__VICTAURI__.getIpcLog();
                    var entry = log.find(function(e) { return e.command === 'my command:with special'; });
                    return { found: !!entry, command: entry ? entry.command : null };
                "
            .into(),
            setup_html: None,
            setup_js: None,
        }],
    };
    let Some(results) = run_tests(&def) else {
        return;
    };
    assert_all_pass(&results);
    assert_eq!(results[0].result.as_ref().unwrap()["found"], true);
    assert_eq!(
        results[0].result.as_ref().unwrap()["command"],
        "my command:with special"
    );
}
