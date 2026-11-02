//! The JS bridge does what it reports (0.10.3 audit): the real bridge under jsdom.
//!
//! Same harness as `bridge_tests.rs`. jsdom has no layout, no hit testing and no Web Animations:
//! what depends on those (real opacity:0 overlays, an ancestor really on top, a `<label>`-wrapped
//! input being hit, animation scrub, real reloads) is proven only in real Chromium with the
//! scratch harness; here hit testing is stubbed where a case needs it.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use victauri_plugin::js_bridge::{BridgeCapacities, agent_key, init_script};

#[derive(Serialize)]
struct TestDef {
    bridge_script: String,
    setup_html: String,
    setup_js: String,
    tests: Vec<TestCase>,
}

#[derive(Serialize)]
struct TestCase {
    name: String,
    code: String,
    setup_html: String,
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

const SETUP: &str = r"
window.T = function(c, m) { if (!c) throw new Error(m || 'assertion failed'); };
window.B = window.__VICTAURI__;
window.J = function(v) { return JSON.stringify(v); };
window.$ = function(s) { return document.querySelector(s); };
window.refOf = function(s, q) { return B.findElements(Object.assign({css: s, include_hidden: true}, q || {}))[0].ref_id; };
";

fn run(cases: &[(&str, &str, &str)], web_api: bool) -> Option<Vec<TestResult>> {
    if !runner_dir().join("node_modules").join("jsdom").exists() {
        assert!(
            std::env::var_os("CI").is_none()
                && std::env::var_os("VICTAURI_REQUIRE_JSDOM").is_none(),
            "jsdom is not installed: `npm ci` in crates/victauri-plugin/tests/bridge_tests/"
        );
        eprintln!("SKIP: jsdom not installed (run `npm ci` in tests/bridge_tests/)");
        return None;
    }
    let def = TestDef {
        bridge_script: init_script(&BridgeCapacities::default()),
        setup_html: String::new(),
        setup_js: SETUP.to_string(),
        tests: cases
            .iter()
            .map(|(name, html, code)| TestCase {
                name: (*name).to_string(),
                code: code.replace(
                    "AGENT",
                    &format!("window.__VICTAURI__._agent('{}')", agent_key()),
                ),
                setup_html: format!(
                    "<html lang=\"en\"><head><title>t</title></head><body>{html}</body></html>"
                ),
                node_web_api: web_api,
            })
            .collect(),
    };
    let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
    tmp.write_all(serde_json::to_string(&def).expect("json").as_bytes())
        .expect("write");
    tmp.flush().expect("flush");
    let output = Command::new("node")
        .arg(runner_dir().join("run_tests.js"))
        .arg(tmp.path())
        .output()
        .expect("failed to run node");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find(|l| l.starts_with("VICTAURI_RESULTS:"))
        .unwrap_or_else(|| {
            panic!(
                "no results.\nstdout: {stdout}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
    Some(serde_json::from_str(&line["VICTAURI_RESULTS:".len()..]).expect("results json"))
}

fn check(cases: &[(&str, &str, &str)]) {
    let Some(results) = run(cases, false) else {
        return;
    };
    assert_eq!(results.len(), cases.len());
    let failures: Vec<String> = results
        .iter()
        .filter(|r| !r.passed)
        .map(|r| format!("  {}: {}", r.name, r.error.as_deref().unwrap_or("?")))
        .collect();
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}

fn check_web(cases: &[(&str, &str, &str)]) {
    let Some(results) = run(cases, true) else {
        return;
    };
    let failures: Vec<String> = results
        .iter()
        .filter(|r| !r.passed)
        .map(|r| format!("  {}: {}", r.name, r.error.as_deref().unwrap_or("?")))
        .collect();
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}

// ── Refs ─────────────────────────────────────────────────────────────────────

#[test]
fn refs_carry_a_per_page_tag() {
    let code = "var r = B.findElements({css:'button'})[0].ref_id; \
                T(/^[a-z][0-9a-z]{3}e\\d+$/.test(r), 'format: ' + r); \
                T(B.snapshot().tree.indexOf('[' + r + ']') !== -1, 'compact tree uses it'); \
                return r;";
    let cases = [
        ("a", "<button>x</button>", code),
        ("b", "<button>x</button>", code),
    ];
    let Some(results) = run(&cases, false) else {
        return;
    };
    assert!(results.iter().all(|r| r.passed), "{results:?}");
    assert_ne!(
        results[0].result, results[1].result,
        "two page loads must not mint the same ref for the same element"
    );
}

/// A reload leaves the page with no body for a moment, and agents read metrics during it.
#[test]
fn page_reads_survive_a_page_with_no_body() {
    check(&[(
        "no_body",
        "<p>x</p>",
        "document.body.remove(); \
         var m = B.getPerformanceMetrics(); T(m && !m.error, J(m)); \
         var s = B.snapshot(); T(s, 'snapshot'); \
         var a = B.auditAccessibility(); T(a && a.summary, J(a));",
    )]);
}

#[test]
fn a_ref_from_another_page_load_fails_at_once_and_says_so() {
    check(&[
        (
            "click",
            "<button>x</button>",
            "var t0 = Date.now(); var r = await B.click('zzzze1', 5000); \
             T(r.ok === false, J(r)); \
             T(r.error.indexOf('ref not found: zzzze1') === 0, r.error); \
             T(r.error.indexOf('previous page load') !== -1, r.error); \
             T(r.hint === 'CHECK_INPUT', J(r)); \
             T(Date.now() - t0 < 1000, 'must not wait out the timeout');",
        ),
        (
            "bare_counter_means_this_page",
            "<button id=b>x</button>",
            "var n = 0; $('#b').onclick = function() { n++; }; \
             var tagged = refOf('#b'); var bare = tagged.slice(4); T(/^e\\d+$/.test(bare), bare); \
             var r = await B.click(bare, 300); T(r.ok, J(r)); T(n === 1, 'n=' + n); \
             var s = B.getStyles(bare); T(!s.error, J(s)); \
             var gone = await B.click('e99999', 100); \
             T(gone.ok === false && gone.error.indexOf('ref not found: e99999') === 0, J(gone)); \
             T(gone.error.indexOf('previous page load') === -1, J(gone));",
        ),
        (
            "read_tools",
            "<button>x</button>",
            "var r = B.getStyles('zzzze1'); T(r.error.indexOf('previous page load') !== -1, J(r)); \
             var h = B.highlightElement('zzzze1'); T(h.error.indexOf('previous page load') !== -1, J(h)); \
             T(B.getRef('zzzze1') === null, 'getRef');",
        ),
        (
            "own_detached_ref_still_ref_not_found",
            "<button id=b>x</button>",
            "var r = refOf('#b'); $('#b').remove(); var c = await B.click(r, 100); \
             T(c.error.indexOf('ref not found: ' + r) !== -1, J(c)); \
             T(c.error.indexOf('previous page load') === -1, J(c));",
        ),
    ]);
}

// ── Page-replaceable built-ins and prototype pollution ───────────────────────

#[test]
fn page_hooks_on_builtins_cannot_reach_live_state() {
    check(&[
        (
            "map_get_redirect",
            "<button id=c>cancel</button><button id=d>delete</button>",
            "var clicked = []; \
             $('#c').onclick = function(){clicked.push('c')}; $('#d').onclick = function(){clicked.push('d')}; \
             var c = refOf('#c'), d = $('#d'); \
             var og = Map.prototype.get; Map.prototype.get = function(k) { return d; }; \
             var r = await B.click(c, 200); Map.prototype.get = og; \
             T(r.ok && J(clicked) === '[\"c\"]', J(clicked));",
        ),
        (
            "array_push_never_gets_a_log",
            "<div></div>",
            "var saw = 0, op = Array.prototype.push, os = Array.prototype.shift; \
             Array.prototype.push = function() { for (var i=0;i<arguments.length;i++) { var a = arguments[i]; if (a && a.level === 'info') saw++; } return op.apply(this, arguments); }; \
             console.info('x'); alert('y'); history.pushState({}, '', '#z'); \
             Array.prototype.push = op; T(saw === 0, 'hook saw a live log entry'); \
             T(B.getConsoleLogs().length >= 1, 'still logged'); T(B.getDialogLog().length === 1, 'dialog logged');",
        ),
        (
            "foreach_filter_never_get_a_log",
            "<div></div>",
            "console.info('x'); var live = 0, of = Array.prototype.forEach, ofi = Array.prototype.filter; \
             function spy(o) { return function() { if (this.length && this[0] && this[0].level) live++; return o.apply(this, arguments); }; } \
             Array.prototype.forEach = spy(of); Array.prototype.filter = spy(ofi); \
             B.getEventStream(); B.getConsoleLogs(1); \
             Array.prototype.forEach = of; Array.prototype.filter = ofi; T(live === 0, 'live=' + live);",
        ),
        (
            "indexof_cannot_mark_a_call_internal",
            "<div></div>",
            "var oi = String.prototype.indexOf; \
             String.prototype.indexOf = function(s) { return s === 'plugin:victauri|' ? 0 : oi.apply(this, arguments); }; \
             await fetch('http://ipc.localhost/greet', {method:'POST', body:'{}'}); \
             String.prototype.indexOf = oi; \
             T(B.getIpcLog().length === 1, 'call hidden');",
        ),
        (
            "decode_cannot_rename_a_call",
            "<div></div>",
            "await fetch('http://ipc.localhost/greet', {method:'POST', body:'{}'}); \
             var od = window.decodeURIComponent; window.decodeURIComponent = function() { return 'harmless'; }; \
             var n = B.getIpcLog()[0].command; window.decodeURIComponent = od; T(n === 'greet', n);",
        ),
    ]);
}

#[test]
fn prototype_pollution_cannot_forge_or_hide() {
    check(&[
        (
            "mocked_tag",
            "<div></div>",
            "await fetch('http://ipc.localhost/greet', {method:'POST', body:'{}'}); \
             Object.prototype.mocked = true; \
             var e = B.getIpcLog(); var bo = B.getIpcLog(0, {backend_only:true}); delete Object.prototype.mocked; \
             T(e[0].mocked !== true, 'forged tag'); T(bo.length === 1, 'backend_only blinded');",
        ),
        (
            "hide_all",
            "<div></div>",
            "await fetch('http://ipc.localhost/greet', {method:'POST', body:'{}'}); \
             Object.prototype.superseded_by_post_message = true; \
             var n = B.getIpcLog().length; delete Object.prototype.superseded_by_post_message; T(n === 1, 'hidden');",
        ),
        (
            "plant_quit",
            "<div></div>",
            "Object.prototype.transport = 'post_message'; Object.prototype.ipc_path = 'quit_app'; \
             try { await fetch('data:,x'); } catch (e) {} \
             var l = B.getIpcLog(); delete Object.prototype.transport; delete Object.prototype.ipc_path; \
             T(l.length === 0, J(l));",
        ),
        (
            "thrown_eval_stays_an_error",
            "<div></div>",
            "var sent = []; window.__TAURI_INTERNALS__ = {invoke: function(c, a) { sent.push(a.result); return Promise.resolve(); }}; \
             Object.prototype.__victauri_type = 'value'; Object.prototype.__victauri_ok = 'FORGED'; \
             B._evalBegin('id1'); await B._evalSettle('id1', {__victauri_err: 'real'}); \
             delete Object.prototype.__victauri_type; delete Object.prototype.__victauri_ok; \
             T(sent[0].indexOf('real') !== -1 && sent[0].indexOf('FORGED') === -1, sent[0]);",
        ),
        (
            "rule_fields",
            "<div></div>",
            "Object.prototype.method = 'DELETE'; \
             var r = AGENT.addRoute({pattern:'/x', action:'block'}); delete Object.prototype.method; \
             T(r.ok && r.rule.method === null, J(r));",
        ),
        (
            "find_query_fields",
            "<button>x</button>",
            "Object.prototype.role = 'nope'; var n = B.findElements({tag:'button'}).length; \
             delete Object.prototype.role; T(n === 1, 'n=' + n);",
        ),
        (
            "waitfor_opts",
            "<div></div>",
            "Object.prototype.condition = 'selector'; Object.prototype.value = '#never'; \
             var r = await B.waitFor({condition:'url', value:'localhost', timeout_ms: 50}); \
             delete Object.prototype.condition; delete Object.prototype.value; T(r.ok, J(r));",
        ),
    ]);
}

// ── Inputs ───────────────────────────────────────────────────────────────────

#[test]
fn type_does_not_corrupt_sanitized_inputs() {
    let run_type = |typed: &str| {
        format!(
            "var r = await B.type(refOf('input'), {typed:?}, 500); \
             T(r.ok === true, J(r)); T($('input').value === {typed:?}, 'value ' + $('input').value);",
        )
    };
    let c = ["-5", "1.5", "1e3", "2026-01-02", "12:30"].map(run_type);
    check(&[
        ("minus", "<input type=number>", c[0].as_str()),
        ("dec", "<input type=number>", c[1].as_str()),
        ("exp", "<input type=number>", c[2].as_str()),
        ("date", "<input type=date>", c[3].as_str()),
        ("time", "<input type=time>", c[4].as_str()),
        (
            "rejected_is_an_error_and_restores",
            "<input type=number value=7>",
            "var r = await B.type(refOf('input'), 'abc', 200); \
             T(r.ok === false && r.error.indexOf('rejected') !== -1, J(r)); \
             T($('input').value === '7', $('input').value);",
        ),
        (
            "text_keeps_per_char_events",
            "<input>",
            "var n = 0; $('input').addEventListener('input', function(){n++}); \
             var r = await B.type(refOf('input'), 'abc', 200); T(r.ok && n === 3 && $('input').value === 'abc', n);",
        ),
        (
            "surrogate_pair_is_one_key",
            "<input>",
            "var k = []; $('input').addEventListener('keydown', function(e){k.push(e.key)}); \
             await B.type(refOf('input'), '\\uD83D\\uDE00', 200); \
             T(k.length === 1 && k[0] === '\\uD83D\\uDE00', J(k));",
        ),
        (
            "size_cap_names_fill",
            "<input>",
            "var r = await B.type(refOf('input'), new Array(20002).join('a'), 200); \
             T(r.ok === false && r.error.indexOf('use fill') !== -1, J(r)); T($('input').value === '', 'unchanged');",
        ),
        (
            "refuses_checkbox_and_readonly",
            "<input type=checkbox id=c><input id=r readonly>",
            "var a = await B.type(refOf('#c'), 'x', 100); T(a.ok === false, J(a)); \
             var b = await B.type(refOf('#r'), 'x', 100); T(b.ok === false && b.error.indexOf('readonly') !== -1, J(b));",
        ),
    ]);
}

#[test]
fn fill_refuses_what_a_user_could_not_do_and_reads_back() {
    check(&[
        (
            "readonly",
            "<input readonly value=locked>",
            "var r = await B.fill(refOf('input'), 'changed', 100); \
             T(r.ok === false && r.error.indexOf('readonly') !== -1, J(r)); T($('input').value === 'locked');",
        ),
        (
            "checkbox",
            "<input type=checkbox>",
            "var r = await B.fill(refOf('input'), 'yes', 100); \
             T(r.ok === false && r.error.indexOf('click') !== -1, J(r)); T($('input').value === 'on');",
        ),
        (
            "number_bad_keeps_value",
            "<input type=number value=1>",
            "var r = await B.fill(refOf('input'), 'abc', 100); \
             T(r.ok === false && r.error.indexOf('rejected') !== -1, J(r)); T($('input').value === '1', $('input').value);",
        ),
        (
            "date_bad_keeps_value",
            "<input type=date value=2026-01-01>",
            "var r = await B.fill(refOf('input'), 'nope', 100); \
             T(r.ok === false, J(r)); T($('input').value === '2026-01-01', $('input').value);",
        ),
        (
            "good_values_fill",
            "<input type=number id=n><input id=t><textarea id=a></textarea>",
            "var r = await B.fill(refOf('#n'), '-5', 100); T(r.ok && $('#n').value === '-5', J(r)); \
             r = await B.fill(refOf('#t'), 'hey', 100); T(r.ok && $('#t').value === 'hey', J(r)); \
             r = await B.fill(refOf('#a'), 'ta', 100); T(r.ok && $('#a').value === 'ta', J(r));",
        ),
    ]);
}

#[test]
fn contenteditable_and_iframe_controls_work() {
    check(&[
        (
            "type_contenteditable",
            "<div contenteditable=true id=c></div>",
            "var r = await B.type(refOf('#c'), 'hi', 200); T(r.ok, J(r)); T($('#c').textContent === 'hi', $('#c').textContent);",
        ),
        (
            "fill_empty_attr_and_plaintext",
            "<div contenteditable id=a></div><div contenteditable=plaintext-only id=b></div>",
            "var r = await B.fill(refOf('#a'), 'x', 200); T(r.ok, J(r)); \
             r = await B.fill(refOf('#b'), 'y', 200); T(r.ok, J(r)); \
             T($('#a').textContent === 'x' && $('#b').textContent === 'y');",
        ),
        (
            "contenteditable_false_refused",
            "<div contenteditable=false id=a></div>",
            "var r = await B.fill(refOf('#a'), 'x', 100); T(r.ok === false, J(r));",
        ),
        (
            "iframe_textarea",
            "<iframe id=f></iframe>",
            "var d = $('#f').contentDocument; d.body.innerHTML = '<textarea id=t></textarea>'; \
             var w = $('#f').contentWindow; w.document.elementFromPoint = function(){return null}; w.Element.prototype.getBoundingClientRect = Element.prototype.getBoundingClientRect; \
             var r = B.findElements({tag:'textarea'})[0].ref_id; \
             var a = await B.fill(r, 'via fill', 300); T(a.ok, J(a)); T(d.getElementById('t').value === 'via fill'); \
             var b = await B.type(r, '!', 300); T(b.ok, J(b)); T(d.getElementById('t').value === 'via fill!');",
        ),
    ]);
}

#[test]
fn select_option_reports_what_happened() {
    let html = "<select><option value=a>A</option><option value=b selected>B</option>\
                <option value=c disabled>C</option><optgroup disabled><option value=d>D</option></optgroup></select>";
    check(&[
        (
            "missing",
            html,
            "var r = await B.selectOption(refOf('select'), ['nope'], 100); \
             T(r.ok === false && r.error.indexOf('nope') !== -1, J(r)); T($('select').value === 'b', $('select').value);",
        ),
        (
            "empty",
            html,
            "var r = await B.selectOption(refOf('select'), [], 100); \
             T(r.ok === false, J(r)); T($('select').value === 'b');",
        ),
        (
            "disabled_option",
            html,
            "var r = await B.selectOption(refOf('select'), ['c'], 100); \
             T(r.ok === false && r.error.indexOf('disabled') !== -1, J(r)); T($('select').value === 'b');",
        ),
        (
            "disabled_optgroup",
            html,
            "var r = await B.selectOption(refOf('select'), ['d'], 100); \
             T(r.ok === false && r.error.indexOf('disabled') !== -1, J(r)); T($('select').value === 'b');",
        ),
        (
            "events_input_then_change",
            html,
            "var ev = []; $('select').addEventListener('input', function(){ev.push('input')}); \
             $('select').addEventListener('change', function(){ev.push('change')}); \
             var r = await B.selectOption(refOf('select'), ['a'], 100); \
             T(r.ok && $('select').value === 'a', J(r)); T(J(ev) === '[\"input\",\"change\"]', J(ev));",
        ),
    ]);
}

// ── Actionability ────────────────────────────────────────────────────────────

#[test]
fn fieldset_disabled_is_disabled_everywhere() {
    let html = "<fieldset disabled><button id=b>x</button></fieldset><button id=a aria-disabled=true>y</button>";
    check(&[
        (
            "click_refused",
            html,
            "var r = await B.click(refOf('#b'), 100); T(r.ok === false && r.error.indexOf('disabled') !== -1, J(r));",
        ),
        (
            "find_enabled_field",
            html,
            "var f = B.findElements({css:'#b'})[0]; T(f.enabled === false, J(f)); \
             var g = B.findElements({css:'#a'})[0]; T(g.enabled === false, J(g)); \
             T(B.findElements({css:'button', enabled:true}).length === 0, 'filter enabled'); \
             T(B.findElements({css:'button', enabled:false}).length === 2, 'filter disabled');",
        ),
        (
            "snapshots",
            html,
            "var t = B.snapshot().tree; T(t.indexOf('[disabled]') !== -1, t); \
             var tree = J(B.snapshot('json').tree); T(tree.indexOf('\"enabled\":false') !== -1, tree);",
        ),
    ]);
}

/// A text query returns the deepest match, often a span: it must report its control's state.
#[test]
fn content_of_a_disabled_control_is_disabled() {
    let html = "<button id=b disabled><span id=s>Save</span></button>\
                <div role=button aria-disabled=true><span id=a>Open</span></div>\
                <fieldset disabled><span id=f>note</span></fieldset>\
                <button id=ok><span id=live>Go</span></button>";
    check(&[(
        "state",
        html,
        "T(B.findElements({css:'#s'})[0].enabled === false, 'span in disabled button'); \
         T(B.findElements({css:'#a'})[0].enabled === false, 'span in aria-disabled'); \
         T(B.findElements({css:'#live'})[0].enabled === true, 'span in live button'); \
         T(B.findElements({css:'#f'})[0].enabled === true, 'plain text in a fieldset is no control'); \
         var r = await B.click(refOf('#s'), 100); T(r.ok === false && r.error.indexOf('disabled') !== -1, J(r));",
    )]);
}

#[test]
fn click_handles_svg_opacity_and_hit_targets() {
    check(&[
        (
            "svg",
            "<svg id=s width=10 height=10></svg>",
            "var n = 0; $('#s').addEventListener('click', function(e){ n++; T(e.bubbles); }); \
             var r = await B.click(refOf('#s'), 200); T(r.ok, J(r)); T(n === 1, 'n=' + n);",
        ),
        (
            "opacity_zero_is_actionable",
            "<input type=checkbox id=c style='opacity:0'>",
            "var r = await B.click(refOf('#c'), 200); T(r.ok, J(r)); T($('#c').checked, 'checked');",
        ),
        (
            "opacity_zero_in_snapshots",
            "<input type=checkbox id=c style='opacity:0'>",
            "var t = B.snapshot().tree; T(t.indexOf('checkbox') !== -1 && t.indexOf('[transparent]') !== -1, t); \
             var j = J(B.snapshot('json').tree); T(j.indexOf('\"transparent\":true') !== -1, j);",
        ),
        (
            "click_lands_on_the_descendant_that_is_hit",
            "<span id=w><input type=checkbox id=c></span>",
            "document.elementFromPoint = function(){ return $('#c'); }; \
             var r = await B.click(refOf('#w'), 200); T(r.ok, J(r)); T($('#c').checked, 'toggled');",
        ),
        (
            "label_of_the_control_on_top_is_not_covering",
            "<label id=l>Name <input type=checkbox id=c></label>",
            "document.elementFromPoint = function(){ return $('#l'); }; \
             var r = await B.click(refOf('#c'), 200); T(r.ok, J(r)); T($('#c').checked, 'toggled');",
        ),
        (
            "other_label_on_top_is_covering",
            "<label id=l for=x>Name</label><input id=x><input type=checkbox id=c>",
            "document.elementFromPoint = function(){ return $('#l'); }; \
             var r = await B.click(refOf('#c'), 150); T(r.ok === false, J(r));",
        ),
    ]);
}

#[test]
fn clobbering_form_controls_cannot_hide_or_fail_a_form() {
    check(&[
        (
            "children",
            "<form id=f><input name=children id=i><button>go</button></form>",
            "var t = B.snapshot().tree; T(t.indexOf('button') !== -1, t); \
             T(B.findElements({tag:'button'}).length === 1, 'find');",
        ),
        (
            "getattribute_and_more",
            "<form><input name=getAttribute><input name=tagName><input name=nodeType><input name=hasAttribute><input name=getBoundingClientRect><input name=shadowRoot><input name=childNodes></form>",
            "var t = B.snapshot().tree; T(t.indexOf('form') !== -1, t); \
             J(B.snapshot('json')); T(B.findElements({role:'form'}).length === 1, 'find');",
        ),
        (
            "disabled_named_control_does_not_disable",
            "<form id=f><input name=disabled></form>",
            "T(B.findElements({css:'#f'})[0].enabled === true, 'enabled');",
        ),
    ]);
}

#[test]
fn visible_children_of_a_visibility_hidden_parent_are_seen() {
    let html = "<div id=p style='visibility:hidden'><span id=g style='visibility:visible'>vis</span><span id=h>no</span></div><div style='display:none'><b id=n>x</b></div>";
    check(&[
        (
            "compact",
            html,
            "var t = B.snapshot().tree; T(t.indexOf('vis') !== -1, t);",
        ),
        (
            "json",
            html,
            "var j = J(B.snapshot('json').tree); T(j.indexOf('\"text\":\"vis\"') !== -1, j);",
        ),
        (
            "find",
            html,
            "T(B.findElements({css:'#g'}).length === 1, 'child'); \
             T(B.findElements({css:'#p'}).length === 0, 'parent hidden'); \
             T(B.findElements({css:'#n'}).length === 0, 'under display:none');",
        ),
    ]);
}

#[test]
fn synthetic_events_cross_shadow_boundaries() {
    let setup = "var h = $('#h'); var s = h.attachShadow({mode:'open'}); s.innerHTML = '<input id=i>'; \
                 var inner = s.getElementById('i'); var seen = []; \
                 ['input','keydown','dblclick','mouseover'].forEach(function(t){ h.addEventListener(t, function(){seen.push(t)}); }); ";
    let fill = format!(
        "{setup} var r = B.findElements({{tag:'input'}})[0].ref_id; \
         await B.fill(r, 'x', 200); T(seen.indexOf('input') !== -1, J(seen)); \
         await B.doubleClick(r, 200); T(seen.indexOf('dblclick') !== -1, J(seen)); \
         await B.hover(r, 200); T(seen.indexOf('mouseover') !== -1, J(seen));"
    );
    let press = format!(
        "{setup} var got = []; inner.addEventListener('keydown', function(e){{got.push(e.key)}}); \
         inner.focus(); B.pressKey('a'); T(J(got) === '[\"a\"]', J(got)); T(seen.indexOf('keydown') !== -1, J(seen));"
    );
    check(&[
        ("fill_dbl_hover", "<div id=h></div>", fill.as_str()),
        ("presskey_descends", "<div id=h></div>", press.as_str()),
    ]);
}

// ── find_elements contract ───────────────────────────────────────────────────

#[test]
fn find_text_returns_only_the_deepest_matches() {
    let deep = "<div><div><div><div><div><div><div><div><div><div><div><div><button id=b>Save</button></div></div></div></div></div></div></div></div></div></div></div></div>";
    check(&[
        (
            "button_within_default_max",
            deep,
            "var r = B.findElements({text:'Save'}); T(r.length === 1 && r[0].tag === 'button', J(r));",
        ),
        (
            "click_first_match_hits_it",
            deep,
            "var n = 0; $('#b').onclick = function(){n++}; \
             var c = await B.click(B.findElements({text:'save'})[0].ref_id, 200); T(c.ok && n === 1, J(c));",
        ),
        (
            "siblings_in_document_order",
            "<ul><li>a save</li><li><b>save b</b></li></ul>",
            "var r = B.findElements({text:'save'}); T(J(r.map(function(e){return e.tag})) === '[\"li\",\"b\"]', J(r));",
        ),
        (
            "other_filters_pick_the_deepest_that_satisfies_all",
            "<div id=x role=button><span>Save</span></div>",
            "var r = B.findElements({text:'Save', role:'button'}); T(r.length === 1 && r[0].tag === 'div', J(r));",
        ),
        (
            "text_exact_normalises_full_text",
            "<p id=p>  Hello\n   <b>big</b>   world </p><p>Hello big</p>",
            "var r = B.findElements({text_exact:'Hello big world'}); T(r.length === 1 && r[0].tag === 'p', J(r)); \
             T(B.findElements({text_exact:'Hello'}).length === 0, 'not a substring'); \
             T(B.findElements({text_exact:'hello big world'}).length === 0, 'case sensitive');",
        ),
        (
            "full_text_past_the_result_field",
            "<div id=d></div>",
            "$('#d').textContent = new Array(151).join('x') + 'NEEDLE'; \
             var r = B.findElements({text:'needle'}); T(r.length === 1 && r[0].text.indexOf('NEEDLE') === -1, J(r)); \
             T(B.findElements({text_exact: $('#d').textContent}).length === 1, 'exact over 100 chars');",
        ),
        (
            "script_and_style_text_never_match",
            "<div id=d><script>var Save = 1;</script><style>.Save{}</style></div>",
            "T(B.findElements({text:'Save', include_hidden:true}).length === 0, 'matched code');",
        ),
        (
            "non_text_queries_stay_preorder",
            "<div id=o class=k><div id=i class=k></div></div>",
            "var r = B.findElements({css:'.k'}); T(r.length === 2, J(r));",
        ),
        (
            "legacy_exact_flag_still_works",
            "<button>Save</button><button>Save all</button>",
            "var r = B.findElements({text:'Save', exact:true}); T(r.length === 1 && r[0].text === 'Save', J(r));",
        ),
    ]);
}

#[test]
fn find_attribute_hidden_cap_contract() {
    let rows: String = (0..120).fold(String::new(), |mut s, i| {
        s.push_str(&format!("<li data-row={i}>row {i}</li>"));
        s
    });
    check(&[
        (
            "attribute",
            "<i data-a=1 id=x></i><i data-a=2 id=y></i><i id=z></i>",
            "T(B.findElements({attribute:'data-a', include_hidden:true}).length === 2, 'presence'); \
             var r = B.findElements({attribute:'data-a', attribute_value:'2', include_hidden:true}); \
             T(r.length === 1, J(r));",
        ),
        (
            "attribute_value_needs_attribute",
            "<i></i>",
            "var r = B.findElements({attribute_value:'x'}); T(r.error && r.error.indexOf('attribute_value') !== -1, J(r));",
        ),
        (
            "include_hidden_reports_honest_visible",
            "<div style='display:none'><b id=n>x</b></div><div style='visibility:hidden' id=v></div><div id=s>y</div>",
            "T(B.findElements({css:'#n'}).length === 0, 'pruned by default'); \
             var n = B.findElements({css:'#n', include_hidden:true}); T(n.length === 1 && n[0].visible === false, J(n)); \
             var v = B.findElements({css:'#v', include_hidden:true}); T(v.length === 1 && v[0].visible === false, J(v)); \
             var s = B.findElements({css:'#s', include_hidden:true}); T(s.length === 1 && s[0].visible === true, J(s));",
        ),
        (
            "zero_returns_none",
            "<i id=a></i>",
            "var r = B.findElements({css:'*', max_results:0}); T(Array.isArray(r) && r.length === 0, J(r));",
        ),
        (
            "default_is_ten_and_cap_is_5000",
            rows.as_str(),
            "T(B.findElements({css:'li'}).length === 10, 'default'); \
             T(B.findElements({css:'li', max_results:100000}).length === 120, 'all'); \
             T(B.findElements({css:'li', max_results:50}).length === 50, 'cap');",
        ),
        (
            "filters_apply_before_the_cap",
            rows.as_str(),
            "var r = B.findElements({tag:'li', attribute:'data-row', attribute_value:'119', max_results:1}); \
             T(r.length === 1, J(r)); \
             var t = B.findElements({tag:'li', text_exact:'row 75', max_results:1}); T(t.length === 1, J(t));",
        ),
        (
            "the_5000_cap_is_enforced",
            "<div id=d></div>",
            "for (var i = 0; i < 5100; i++) $('#d').appendChild(document.createElement('i')); \
             T(B.findElements({tag:'i', max_results:999999}).length === 5000, 'cap');",
        ),
    ]);
}

/// A locator is generated from a snapshot's name, so both tools must give the same one.
#[test]
fn snapshots_report_the_name_find_elements_matches() {
    let html = "<label for=i>Your name</label><input id=i placeholder='Enter...'>\
                <button id=b title=Hint>Save</button><button id=x aria-label=Close>x</button>";
    check(&[(
        "same_name",
        html,
        "function walk(n, out) { if (n.tag) out.push(n); (n.children || []).forEach(function(c){ walk(c, out); }); return out; } \
         var nodes = walk(B.snapshot('json').tree, []); \
         ['#i', '#b', '#x'].forEach(function(s) { \
           var f = B.findElements({css: s})[0]; \
           var n = nodes.filter(function(x){ return x.ref_id === f.ref_id; })[0]; \
           T(n && f.name && n.name === f.name, s + ' ' + J(n) + ' vs ' + J(f)); \
           T(B.findElements({role: f.role, name: n.name}).length >= 1, 'found by the snapshot name'); \
         }); \
         var t = B.snapshot().tree; T(t.indexOf('\"Your name\"') !== -1, t); T(t.indexOf('\"Enter...\"') === -1, t);",
    )]);
}

#[test]
fn accessible_name_is_one_computation() {
    check(&[
        (
            "aria_label_first",
            "<button aria-label='A' title=T>text</button>",
            "T(B.findElements({tag:'button'})[0].name === 'A');",
        ),
        (
            "labelledby",
            "<span id=l1>First</span><span id=l2>Second</span><button aria-labelledby='l1 l2' id=b>x</button>",
            "T(B.findElements({css:'#b'})[0].name === 'First Second'); \
             T(B.findElements({name:'first second'}).length === 1, 'filter');",
        ),
        (
            "associated_label",
            "<label for=i>Email</label><input id=i><label>Wrap <input id=w></label>",
            "T(B.findElements({css:'#i'})[0].name === 'Email'); \
             T(B.findElements({css:'#w'})[0].name === 'Wrap'); \
             T(B.findElements({name:'email'}).length === 1, 'filter');",
        ),
        (
            "text_for_buttons_links_summary",
            "<button id=b>Submit</button><a href=# id=a>Home</a><details><summary id=s>More</summary></details><div role=tab id=t>Tab1</div>",
            "['#b','#a','#s','#t'].forEach(function(s){ var e = B.findElements({css:s})[0]; T(e.name, s + J(e)); }); \
             var r = B.findElements({role:'button', name:'Submit'}); T(r.length === 1 && r[0].name === 'Submit', J(r));",
        ),
        (
            "no_name_from_text_for_a_div",
            "<div id=d>hello</div>",
            "T(B.findElements({css:'#d'})[0].name === null);",
        ),
        (
            "title_then_placeholder",
            "<input id=t title=TT placeholder=PP><input id=p placeholder=PP>",
            "T(B.findElements({css:'#t'})[0].name === 'TT'); T(B.findElements({css:'#p'})[0].name === 'PP'); \
             T(B.findElements({name:'pp'}).length === 1, 'filter on the computed name');",
        ),
        (
            "submit_input_value",
            "<input type=submit value=Send id=s><img alt=Logo id=i src=x>",
            "T(B.findElements({css:'#s'})[0].name === 'Send'); T(B.findElements({css:'#i'})[0].name === 'Logo');",
        ),
    ]);
}

// ── waitFor + page text ──────────────────────────────────────────────────────

#[test]
fn wait_for_survives_a_missing_body() {
    check(&[
        (
            "no_body_yet_is_not_met",
            "<div></div>",
            "var body = document.body; document.documentElement.removeChild(body); \
             setTimeout(function(){ document.documentElement.appendChild(body); body.textContent = 'later'; }, 60); \
             var r = await B.waitFor({condition:'text', value:'later', timeout_ms:2000, poll_ms:10}); T(r.ok, J(r));",
        ),
        (
            "body_disappearing_does_not_hang",
            "<div>x</div>",
            "setTimeout(function(){ document.documentElement.removeChild(document.body); }, 30); \
             var r = await Promise.race([ B.waitFor({condition:'text', value:'never', timeout_ms:200, poll_ms:10}), \
               new Promise(function(res){ setTimeout(function(){ res('HUNG'); }, 1500); }) ]); \
             T(r !== 'HUNG' && r.ok === false && r.error.indexOf('timeout') === 0, J(r));",
        ),
        (
            "invalid_selector_ends_it",
            "<div></div>",
            "var r = await Promise.race([ B.waitFor({condition:'selector', value:'##', timeout_ms:5000, poll_ms:10}), \
               new Promise(function(res){ setTimeout(function(){ res('HUNG'); }, 1500); }) ]); \
             T(r !== 'HUNG' && r.ok === false, J(r));",
        ),
    ]);
}

#[test]
fn get_page_text_reads_the_whole_page() {
    check(&[
        (
            "text_across_elements_and_shadow",
            "<p>Fatal <b>crash</b></p><div id=h></div><iframe id=f></iframe>",
            "$('#h').attachShadow({mode:'open'}).innerHTML = '<style>.x{}</style><i>in shadow</i>'; \
             $('#f').contentDocument.body.innerHTML = '<u>in frame</u>'; \
             var t = B.getPageText(); \
             T(t.indexOf('Fatal crash') !== -1, t); T(t.indexOf('in shadow') !== -1, t); T(t.indexOf('in frame') !== -1, t); \
             T(t.indexOf('.x{}') === -1, 'style text: ' + t);",
        ),
        (
            "no_body_is_empty",
            "<p>x</p>",
            "document.documentElement.removeChild(document.body); T(B.getPageText() === '');",
        ),
    ]);
}

// ── Route rules ──────────────────────────────────────────────────────────────

#[test]
fn route_rules_are_agent_only_and_validated() {
    check(&[
        (
            "page_cannot_add",
            "<div></div>",
            "T(B.addRoute === undefined, 'page-visible addRoute'); \
             T(typeof AGENT.addRoute === 'function', 'agent op');",
        ),
        (
            "invalid_regex",
            "<div></div>",
            "var r = AGENT.addRoute({pattern:'(', match_type:'regex', action:'block'}); \
             T(r.ok === false && r.error.indexOf('regex') !== -1, J(r)); T(B.getRouteRules().length === 0);",
        ),
        (
            "headers_not_an_object",
            "<div></div>",
            "var r = AGENT.addRoute({pattern:'/x', headers:'notanobject'}); \
             T(r.ok === false && r.error.indexOf('headers') !== -1, J(r));",
        ),
        (
            "unknown_action_and_match_type",
            "<div></div>",
            "var r = AGENT.addRoute({pattern:'/x', action:'explode'}); T(r.ok === false, J(r)); \
             r = AGENT.addRoute({pattern:'/x', action:'block', match_type:'fuzzy'}); T(r.ok === false, J(r));",
        ),
        (
            "glob_is_linear_on_a_big_url",
            "<div></div>",
            "AGENT.addRoute({pattern:'*/api/*/items/*/x', match_type:'glob', action:'block'}); \
             var big = 'data:text/plain,' + new Array(1048577).join('/api/'.charAt(0)); \
             var t0 = Date.now(); try { await fetch(big); } catch (e) {} \
             T(Date.now() - t0 < 500, 'took ' + (Date.now() - t0));",
        ),
        (
            "glob_still_matches",
            "<div></div>",
            "AGENT.addRoute({pattern:'*/api/*/items/*/x?q=*', match_type:'glob', action:'block'}); \
             var e = null; try { await fetch('http://h/api/1/items/2/x?q=3'); } catch (x) { e = x; } T(e, 'blocked'); \
             var e2 = null; try { await fetch('http://h/api/1/items/2/y?q=3'); } catch (x) { e2 = x; } T(!e2, 'not blocked'); \
             AGENT.clearRoutes(); AGENT.addRoute({pattern:'http://h/a*', match_type:'glob', action:'block'}); \
             var e3 = null; try { await fetch('http://h/abc'); } catch (x) { e3 = x; } T(e3, 'prefix glob'); \
             var e4 = null; try { await fetch('http://x/h/abc'); } catch (x) { e4 = x; } T(!e4, 'anchored');",
        ),
        (
            "opaque_big_url_skips_regex",
            "<div></div>",
            "AGENT.addRoute({pattern:'^(a+)+$', match_type:'regex', action:'block'}); \
             var big = 'data:,' + new Array(9001).join('a') + '!'; \
             var t0 = Date.now(); try { await fetch(big); } catch (e) {} T(Date.now() - t0 < 500, 'regex ran');",
        ),
        (
            "xhr_delay_abort_cancels",
            "<div></div>",
            "AGENT.addRoute({pattern:'/slow', action:'delay', delay_ms: 80}); \
             var ev = []; var x = new XMLHttpRequest(); x.open('GET', 'http://127.0.0.1:9/slow'); \
             ['abort','loadend','load','error'].forEach(function(t){ x.addEventListener(t, function(){ev.push(t)}); }); \
             x.send(); x.abort(); await new Promise(function(r){setTimeout(r, 200)}); \
             T(J(ev) === '[\"abort\",\"loadend\"]', J(ev)); \
             var e = B.getNetworkLog()[0]; T(e.status === 'aborted', J(e));",
        ),
        (
            "xhr_block_fires_readystatechange",
            "<div></div>",
            "AGENT.addRoute({pattern:'/b', action:'block'}); var ev = []; var x = new XMLHttpRequest(); \
             x.open('GET', 'http://127.0.0.1:9/b'); \
             ['readystatechange','error','loadend'].forEach(function(t){ x.addEventListener(t, function(){ev.push(t)}); }); \
             ev.length = 0; x.send(); await new Promise(function(r){setTimeout(r, 30)}); \
             T(J(ev) === '[\"readystatechange\",\"error\",\"loadend\"]', J(ev));",
        ),
    ]);
}

#[test]
fn route_mocks_with_web_apis() {
    check_web(&[
        (
            "content_type_header_not_doubled",
            "<div></div>",
            "AGENT.addRoute({pattern:'/m', action:'fulfill', body:'<p>', headers:{'Content-Type':'text/html'}}); \
             var r = await fetch('http://h/m'); var ct = r.headers.get('content-type'); T(ct === 'text/html', ct);",
        ),
        (
            "delayed_fulfill_honours_abort",
            "<div></div>",
            "AGENT.addRoute({pattern:'/m', action:'fulfill', body:'{}', delay_ms: 3000}); \
             var c = new AbortController(); var t0 = Date.now(); var err = null; \
             var p = fetch('http://h/m', {signal: c.signal}).catch(function(e){ err = e; }); \
             setTimeout(function(){ c.abort(); }, 20); await p; \
             T(err && err.name === 'AbortError', 'got ' + err); T(Date.now() - t0 < 1000, 'waited'); \
             T(B.getNetworkLog()[0].status === 'aborted', J(B.getNetworkLog()));",
        ),
        (
            "delay_rule_honours_abort",
            "<div></div>",
            "AGENT.addRoute({pattern:'/d', action:'delay', delay_ms: 3000}); \
             var c = new AbortController(); var t0 = Date.now(); var err = null; \
             var p = fetch('http://h/d', {signal: c.signal}).catch(function(e){ err = e; }); \
             setTimeout(function(){ c.abort(); }, 20); await p; \
             T(err && err.name === 'AbortError', 'got ' + err); T(Date.now() - t0 < 1000, 'waited');",
        ),
    ]);
}

// ── Logs ─────────────────────────────────────────────────────────────────────

#[test]
fn logs_store_bounded_strings() {
    check(&[
        (
            "navigation_url",
            "<div></div>",
            "var l0 = B.getNavigationLog().length; \
             history.pushState({}, '', '#' + new Array(100001).join('x')); \
             var e = B.getNavigationLog()[l0]; T(e.url.length < 4000, 'len ' + e.url.length);",
        ),
        (
            "dialog_message",
            "<div></div>",
            "alert(new Array(100001).join('x')); confirm(new Array(100001).join('y')); prompt(new Array(100001).join('z')); \
             var d = B.getDialogLog(); T(d.length === 3, J(d.length)); \
             d.forEach(function(e){ T(e.message.length < 5000, 'len ' + e.message.length); });",
        ),
    ]);
}

// ── Serialization, recorder, audit, dialogs, keys ────────────────────────────

#[test]
fn serialization_runs_a_getter_once() {
    check(&[(
        "getter_once",
        "<div></div>",
        "var n = 0; var o = {}; Object.defineProperty(o, 'g', {enumerable:true, get:function(){n++; return 1;}}); \
         var sent = []; window.__TAURI_INTERNALS__ = {invoke:function(c,a){sent.push(a.result); return Promise.resolve();}}; \
         B._evalBegin('i'); await B._evalSettle('i', {__victauri_ok:o, __victauri_type:'value'}); T(n === 1, 'n=' + n); \
         n = 0; Object.prototype.toJSON = function(){return 'forged'}; B._evalBegin('j'); await B._evalSettle('j', {__victauri_ok:{a:1}, __victauri_type:'value'}); \
         delete Object.prototype.toJSON; T(sent[1].indexOf('forged') === -1, sent[1]);",
    )]);
}

#[test]
fn sweep_recorder_validates_and_disarms() {
    check(&[
        (
            "bad_selector",
            "<div></div>",
            "var r = AGENT.installSweepRecorder('##bad'); T(r.error && r.installed !== true, J(r));",
        ),
        (
            "disarm",
            "<div></div>",
            "var r = AGENT.installSweepRecorder('div'); T(r.installed, J(r)); \
             var d = AGENT.disarmSweep(); T(d.disarmed === true, J(d)); \
             var s = AGENT.readSweep(false); T(s.armed === false, J(s)); \
             T(AGENT.disarmSweep().disarmed === false, 'twice');",
        ),
    ]);
}

#[test]
fn accessibility_audit_accepts_valid_markup() {
    check(&[
        (
            "valid",
            "<label>Name <input id=w></label><input type=submit value=Send><input type=reset value=R><input type=button value=B>\
         <a href=#><img alt=Home src=x></a><a href=#><svg aria-label=Icon></svg></a>",
            "var v = B.auditAccessibility().violations.filter(function(x){return x.rule === 'input-label' || x.rule === 'link-name'}); \
         T(v.length === 0, J(v));",
        ),
        (
            "still_flags",
            "<input id=i><a href=#></a><input type=submit>",
            "var v = B.auditAccessibility().violations; \
         T(v.filter(function(x){return x.rule === 'input-label'}).length === 2, J(v)); \
         T(v.filter(function(x){return x.rule === 'link-name'}).length === 1, J(v));",
        ),
    ]);
}

#[test]
fn dialogs_and_keys_are_exact() {
    check(&[
        (
            "prompt_explicit_empty",
            "<div></div>",
            "AGENT.setDialogAutoResponse('prompt', 'accept', ''); var r = prompt('m', 'dflt'); T(r === '', J(r));",
        ),
        (
            "prompt_no_text_uses_default",
            "<div></div>",
            "AGENT.setDialogAutoResponse('prompt', 'accept', null); var r = prompt('m', 'dflt'); T(r === 'dflt', J(r));",
        ),
        (
            "alert_zero",
            "<div></div>",
            "alert(0); alert(); var d = B.getDialogLog(); T(d[0].message === '0' && d[1].message === '', J(d));",
        ),
        (
            "plus_with_modifier",
            "<input id=i>",
            "var k = []; document.addEventListener('keydown', function(e){k.push([e.key, e.ctrlKey])}); $('#i').focus(); \
             B.pressKey('Control++'); T(J(k) === '[[\"Control\",true],[\"+\",true]]', J(k));",
        ),
        (
            "code_and_keycode",
            "<input id=i>",
            "var k = []; document.addEventListener('keydown', function(e){k.push([e.key, e.code, e.keyCode])}); $('#i').focus(); \
             B.pressKey('Control+s'); B.pressKey('Enter'); B.pressKey('a'); B.pressKey('7'); B.pressKey('ArrowUp'); \
             var c = k.map(function(x){return x[1]}); \
             T(c.indexOf('KeyS') !== -1 && c.indexOf('Enter') !== -1 && c.indexOf('KeyA') !== -1 && c.indexOf('Digit7') !== -1 && c.indexOf('ArrowUp') !== -1, J(k));",
        ),
    ]);
}

// ── The trusted click probe follows the same rules ──────────────────────────

#[test]
fn trusted_probe_matches_the_actionability_rules() {
    let probe = |r: &str| {
        serde_json::to_string(&format!(
            "(function() {{ {} }})()",
            victauri_plugin::mcp::trusted_click_probe_js(r)
        ))
        .expect("json")
    };
    let fieldset = format!(
        "var r = refOf('#b'); var p = window.eval({}.replace('__R__', r)); \
         T(p && p.error === 'element is disabled', J(p));",
        probe("__R__")
    );
    let opacity = format!(
        "document.elementFromPoint = function(){{ return $('#c'); }}; \
         $('#c').scrollIntoView = function(){{}}; \
         var r = refOf('#c'); var p = window.eval({}.replace('__R__', r)); \
         T(p && typeof p.x === 'number', J(p));",
        probe("__R__")
    );
    let foreign = format!(
        "var p = window.eval({}); T(p && p.error.indexOf('previous page load') !== -1, J(p));",
        probe("zzzze1")
    );
    check(&[
        (
            "fieldset",
            "<fieldset disabled><button id=b>x</button></fieldset>",
            fieldset.as_str(),
        ),
        (
            "opacity_zero",
            "<input type=checkbox id=c style='opacity:0'>",
            opacity.as_str(),
        ),
        ("foreign_ref", "<i></i>", foreign.as_str()),
    ]);
}

// ── 0.10.3 review round ──────────────────────────────────────────────────────

/// Whether a click reaches an element is the browser's answer at a real point of it, not a guess
/// from boxes: `elementFromPoint` already honours clipping, `clip-path`, `::after` and z-order. jsdom
/// has no layout, so each case states what the browser would answer per point. The shapes are real:
/// SVG geometry API, line boxes, `elementsFromPoint`. Real engines: `examples/gauntlet-app`.
#[test]
fn the_browser_decides_which_point_of_an_element_a_click_reaches() {
    const HELP: &str = r"
        function box(el, x, y, w, h) { el.getBoundingClientRect = function() {
            return { left: x, top: y, right: x + w, bottom: y + h, width: w, height: h, x: x, y: y }; }; }
        function lines(el, list) { el.getClientRects = function() { return list.map(function(l) {
            return { left: l[0], top: l[1], right: l[0] + l[2], bottom: l[1] + l[3], width: l[2], height: l[3] }; }); }; }
        // A circle of radius 40 around (100, 100) in its own coordinates, placed by a real CTM shape.
        function ring(el, dx, dy) {
            box(el, 60 + dx, 60 + dy, 80, 80);
            el.getTotalLength = function() { return 2 * Math.PI * 40; };
            el.getPointAtLength = function(s) { var t = s / 40; return { x: 100 + 40 * Math.cos(t), y: 100 + 40 * Math.sin(t) }; };
            el.getScreenCTM = function() { return { a: 1, b: 0, c: 0, d: 1, e: dx, f: dy }; };
        }
        function onRing(x, y, dx, dy) { return Math.abs(Math.hypot(x - 100 - dx, y - 100 - dy) - 40) < 1; }
    ";
    let stroke_only = format!(
        r"{HELP} var s = $('#s'); ring(s, 20, 10);
        var n = 0; s.addEventListener('click', function() {{ n++; }});
        document.elementFromPoint = function(x, y) {{ return onRing(x, y, 20, 10) ? s : $('#a'); }};
        var r = await B.click(refOf('#s'), 150); T(r.ok, J(r)); T(n === 1, 'n=' + n);
        var p = B.hitPoint(s, true); T(onRing(p.x, p.y, 20, 10), 'the point is not on the stroke: ' + J(p));"
    );
    let stroke_clipped_away = format!(
        r"{HELP} var s = $('#s'); ring(s, 0, 0);
        var n = 0; s.addEventListener('click', function() {{ n++; }});
        document.elementFromPoint = function() {{ return $('#a'); }};
        var r = await B.click(refOf('#s'), 150);
        T(r.ok === false && r.error.indexOf('covered') !== -1, J(r)); T(n === 0);
        T(B.hitPoint(s, true).error, 'no point');"
    );
    let every_line_box = format!(
        r"{HELP} var l = $('#l'); box(l, 0, 0, 100, 60);
        lines(l, [[80, 0, 20, 16], [0, 22, 100, 16], [0, 44, 30, 16]]);
        var n = 0; l.addEventListener('click', function(e) {{ e.preventDefault(); n++; }});
        document.elementFromPoint = function(x, y) {{ return (Math.floor(x) === 15 && Math.floor(y) === 52) ? l : $('#p'); }};
        var r = await B.click(refOf('#l'), 150); T(r.ok, J(r)); T(n === 1, 'n=' + n);
        var p = B.hitPoint(l, true); T(p.x === 15 && p.y === 52, J(p));"
    );
    let wrapped_over_a_neighbour = format!(
        r"{HELP} var l = $('#l'); box(l, 0, 0, 100, 40);
        lines(l, [[80, 0, 20, 16], [0, 24, 30, 16]]);
        document.elementFromPoint = function(x) {{ return x === 90 ? l : $('#o'); }};
        var r = await B.click(refOf('#l'), 150); T(r.ok, J(r));"
    );
    let wrapped_covered = format!(
        r"{HELP} var l = $('#l'); box(l, 0, 0, 100, 40);
        lines(l, [[80, 0, 20, 16], [0, 24, 30, 16]]);
        document.elementFromPoint = function() {{ return $('#o'); }};
        var r = await B.click(refOf('#l'), 150);
        T(r.ok === false && r.error.indexOf('covered by span#o') !== -1, J(r));"
    );
    let hole_at_the_centre = format!(
        r"{HELP} var b = $('#b'); box(b, 0, 0, 80, 80);
        var n = 0; b.onclick = function() {{ n++; }};
        document.elementFromPoint = function(x, y) {{ return (x > 30 && x < 50 && y > 30 && y < 50) ? $('#a') : b; }};
        var r = await B.click(refOf('#b'), 150); T(r.ok, J(r)); T(n === 1, 'n=' + n);
        var p = B.hitPoint(b, true); T(p.x === 8 && p.y === 8, 'the first grid point: ' + J(p));"
    );
    let clipped_to_a_corner = format!(
        r"{HELP} var b = $('#b'); box(b, 0, 0, 100, 40);
        var n = 0; b.onclick = function() {{ n++; }};
        document.elementFromPoint = function(x) {{ return x < 40 ? b : $('#a'); }};
        var r = await B.click(refOf('#b'), 150); T(r.ok, J(r)); T(n === 1, 'n=' + n);"
    );
    let under_the_parents_after = format!(
        r"{HELP} box($('#b'), 50, 50, 20, 20);
        var n = 0; $('#b').onclick = function() {{ n++; }};
        document.elementFromPoint = function() {{ return $('#a'); }};
        document.elementsFromPoint = function() {{ return [$('#a'), $('#b')]; }};
        var r = await B.click(refOf('#b'), 150);
        T(r.ok === false && r.error.indexOf('covered') !== -1, J(r)); T(n === 0);"
    );
    let sibling_at_the_centre = format!(
        r"{HELP} var b = $('#b'); box(b, 0, 0, 100, 40);
        var n = 0; b.onclick = function() {{ n++; }};
        document.elementFromPoint = function(x, y) {{ return (x === 50 && y === 20) ? $('#o') : b; }};
        var r = await B.click(refOf('#b'), 150);
        T(r.ok === false && r.error.indexOf('covered by span#o') !== -1, J(r)); T(n === 0);"
    );
    let sibling_at_an_svg_centre = format!(
        r"{HELP} var s = $('#s'); ring(s, 0, 0);
        document.elementFromPoint = function(x, y) {{ return onRing(x, y, 0, 0) ? s : $('#d'); }};
        var r = await B.click(refOf('#s'), 150); T(r.ok, 'a dot inside a ring: ' + J(r));"
    );
    let off_screen_points_do_not_count = format!(
        r"{HELP} var b = $('#b'); box(b, -500, 10, 100, 40);
        var asked = 0; document.elementFromPoint = function(x, y) {{ if (x < 0) {{ asked++; return b; }} return $('#a'); }};
        var r = await B.click(refOf('#b'), 150); T(r.ok === false, J(r));"
    );
    let content_of_a_control = format!(
        r"{HELP} box($('#b'), 0, 0, 200, 40); box($('#s'), 50, 10, 40, 20);
        var n = 0; $('#b').onclick = function() {{ n++; }};
        document.elementFromPoint = function() {{ return $('#b'); }};
        document.elementsFromPoint = function() {{ return [$('#b'), $('#s'), document.body]; }};
        var r = await B.click(refOf('#s'), 150); T(r.ok, J(r)); T(n === 1, 'n=' + n);"
    );
    let content_clipped_below_a_control = format!(
        r"{HELP} box($('#b'), 0, 0, 200, 40); box($('#s'), 50, 10, 40, 20);
        document.elementFromPoint = function() {{ return $('#b'); }};
        document.elementsFromPoint = function() {{ return [$('#b'), document.body]; }};
        var r = await B.click(refOf('#s'), 150);
        T(r.ok === false && r.error.indexOf('covered') !== -1, J(r));"
    );
    let content_under_a_plain_ancestor = format!(
        r"{HELP} box($('#a'), 0, 0, 200, 40); box($('#b'), 50, 10, 40, 20);
        document.elementFromPoint = function() {{ return $('#a'); }};
        document.elementsFromPoint = function() {{ return [$('#a'), $('#b')]; }};
        var r = await B.click(refOf('#b'), 150); T(r.ok === false, 'a div is not a control: ' + J(r));"
    );
    let a_real_click_does_not_land_on_content_below_a_control = format!(
        r"{HELP} box($('#b'), 0, 0, 200, 40); box($('#s'), 50, 10, 40, 20);
        document.elementFromPoint = function() {{ return $('#b'); }};
        document.elementsFromPoint = function() {{ return [$('#b'), $('#s')]; }};
        T(B.hitPoint($('#s'), true).error, 'strict: the OS click lands on the button');
        T(!B.hitPoint($('#s'), false).error, 'not strict: its content');"
    );
    let empty_link_around_a_float = format!(
        r"{HELP} var a = $('#e'), c = $('#c'); box(a, 10, 10, 0, 0); box(c, 300, 10, 40, 40);
        document.elementFromPoint = function(x, y) {{ return (Math.floor(x) === 320 && Math.floor(y) === 30) ? c : $('#w'); }};
        var r = await B.click(refOf('#e'), 150); T(r.ok, J(r));
        var p = B.hitPoint(a, true); T(p.x === 320 && p.y === 30, J(p));"
    );
    let empty_link_with_nothing = format!(
        r"{HELP} var a = $('#e'); box(a, 10, 10, 0, 0); box($('#c'), 0, 0, 0, 0);
        var r = await B.click(refOf('#e'), 100);
        T(r.ok === false && r.error.indexOf('zero size') !== -1, J(r));"
    );
    let group_with_an_empty_centre = format!(
        r"{HELP} var g = $('#g'), c = $('#r2'); box(g, 0, 0, 100, 100); box($('#r1'), 0, 0, 2, 2); box(c, 90, 90, 10, 10);
        document.elementFromPoint = function(x, y) {{ return (x === 95 && y === 95) ? c : $('#a'); }};
        var r = await B.click(refOf('#g'), 150); T(r.ok, J(r));"
    );
    let ring_html = "<div id=a><svg width=9 height=9><circle id=s r=4></circle></svg></div>";
    let lines_html = "<p id=p><a id=l href='#x'>wrapped</a><span id=o>other</span></p>";
    let button_html = "<div id=a><button id=b>x</button><span id=o>o</span></div>";
    let control_html = "<button id=b><span id=s>Save</span></button>";
    let empty_html = "<div id=w><a id=e href='#x'><img id=c alt=c></a></div>";
    check(&[
        ("stroke_only_svg_uses_its_stroke", ring_html, &stroke_only),
        ("stroke_clipped_away", ring_html, &stroke_clipped_away),
        ("every_line_box_is_tried", lines_html, &every_line_box),
        (
            "wrapped_inline_over_a_neighbour",
            lines_html,
            &wrapped_over_a_neighbour,
        ),
        ("wrapped_inline_covered", lines_html, &wrapped_covered),
        ("hole_at_the_centre", button_html, &hole_at_the_centre),
        ("clipped_to_a_corner", button_html, &clipped_to_a_corner),
        (
            "under_the_parents_after",
            button_html,
            &under_the_parents_after,
        ),
        ("sibling_at_the_centre", button_html, &sibling_at_the_centre),
        (
            "sibling_at_an_svg_centre",
            "<div id=a><svg width=9 height=9><circle id=s r=4></circle><circle id=d r=1></circle></svg></div>",
            &sibling_at_an_svg_centre,
        ),
        (
            "off_screen_points_do_not_count",
            button_html,
            &off_screen_points_do_not_count,
        ),
        ("content_of_a_control", control_html, &content_of_a_control),
        (
            "content_clipped_below_a_control",
            control_html,
            &content_clipped_below_a_control,
        ),
        (
            "content_under_a_plain_ancestor",
            button_html,
            &content_under_a_plain_ancestor,
        ),
        (
            "an_os_click_needs_the_element_itself",
            control_html,
            &a_real_click_does_not_land_on_content_below_a_control,
        ),
        (
            "empty_link_around_a_float",
            empty_html,
            &empty_link_around_a_float,
        ),
        (
            "empty_link_with_nothing",
            empty_html,
            &empty_link_with_nothing,
        ),
        (
            "group_with_an_empty_centre",
            "<div id=a><svg width=9 height=9><g id=g><rect id=r1 width=1 height=1></rect><rect id=r2 width=1 height=1></rect></g></svg></div>",
            &group_with_an_empty_centre,
        ),
    ]);
}

#[test]
fn wait_for_is_not_met_by_a_page_with_no_body_and_rejects_a_bad_selector() {
    check(&[
        (
            "text_gone",
            "<div>x</div>",
            "document.documentElement.removeChild(document.body); \
             var r = await B.waitFor({condition:'text_gone', value:'x', timeout_ms:50, poll_ms:10}); \
             T(r.ok === false && r.error.indexOf('timeout') === 0, J(r));",
        ),
        (
            "selector_gone",
            "<div id=x></div>",
            "document.documentElement.removeChild(document.body); \
             var r = await B.waitFor({condition:'selector_gone', value:'#x', timeout_ms:50, poll_ms:10}); \
             T(r.ok === false && r.error.indexOf('timeout') === 0, J(r));",
        ),
        (
            "text_gone_waits_for_the_new_body",
            "<div>old</div>",
            "var body = document.body; document.documentElement.removeChild(body); \
             setTimeout(function(){ body.textContent = 'new'; document.documentElement.appendChild(body); }, 40); \
             var r = await B.waitFor({condition:'text_gone', value:'old', timeout_ms:2000, poll_ms:10}); T(r.ok, J(r));",
        ),
        (
            "invalid_selector_is_rejected",
            "<div></div>",
            "var r = await B.waitFor({condition:'selector_gone', value:'##', timeout_ms:5000, poll_ms:10}); \
             T(r.ok === false && r.hint === 'CHECK_INPUT', J(r)); \
             T(r.error.indexOf('invalid CSS selector') === 0, J(r));",
        ),
        (
            "ipc_idle_cannot_be_judged_blind",
            "<div></div>",
            "console.warn('IPC custom protocol failed, Tauri will now use the postMessage interface instead'); \
             T(B.getIpcCaptureStatus().capture === 'none', 'capture'); \
             var r = await B.waitFor({condition:'ipc_idle', timeout_ms:2000, poll_ms:10}); \
             T(r.ok === false && r.ipc_capture === 'none', J(r)); T(r.elapsed_ms < 1000, J(r));",
        ),
    ]);
}

#[test]
fn a_reopened_xhr_owns_its_delay_timer() {
    check(&[
        (
            "abort_cancels_the_second_delay_after_the_first_timer_fired",
            "<div></div>",
            "AGENT.addRoute({pattern:'/slow', action:'delay', delay_ms: 300}); \
             var ev = []; var x = new XMLHttpRequest(); \
             ['abort','loadend'].forEach(function(t){ x.addEventListener(t, function(){ev.push(t)}); }); \
             x.open('GET', 'http://127.0.0.1:9/slow'); x.send(); \
             await new Promise(function(r){setTimeout(r, 150)}); \
             x.open('GET', 'http://127.0.0.1:9/slow?2'); x.send(); \
             await new Promise(function(r){setTimeout(r, 200)}); \
             x.abort(); \
             T(J(ev) === '[\"abort\",\"loadend\"]', J(ev)); \
             var log = B.getNetworkLog(); T(log[1].status === 'aborted', J(log));",
        ),
        (
            "abort_after_reopen_with_no_send_fires_nothing",
            "<div></div>",
            "AGENT.addRoute({pattern:'/slow', action:'delay', delay_ms: 300}); \
             var ev = []; var x = new XMLHttpRequest(); \
             ['abort','loadend'].forEach(function(t){ x.addEventListener(t, function(){ev.push(t)}); }); \
             x.open('GET', 'http://127.0.0.1:9/slow'); x.send(); \
             x.open('GET', 'http://127.0.0.1:9/other'); x.abort(); \
             T(J(ev) === '[]', J(ev));",
        ),
    ]);
}

#[test]
fn abort_after_a_route_delay_ended_does_not_rewrite_the_entry() {
    check_web(&[(
        "fulfilled_then_aborted",
        "<div></div>",
        "AGENT.addRoute({pattern:'/m', action:'fulfill', body:'{}', delay_ms: 10}); \
         var c = new AbortController(); \
         var resp = await fetch('http://h/m', {signal: c.signal}); T(resp.ok, 'ok'); \
         var before = B.getNetworkLog()[0].status; c.abort(); \
         var e = B.getNetworkLog()[0]; T(e.status === before && e.status !== 'aborted', J(e));",
    )]);
}

#[test]
fn find_elements_normalises_the_text_needle_and_label_queries_follow_the_rules() {
    check(&[
        (
            "whitespace_in_the_needle",
            "<p id=p>hello   there</p>",
            "var r = B.findElements({text:'hello  \\n there'}); T(r.length === 1 && r[0].tag === 'p', J(r)); \
             T(B.findElements({text:'  '}).length === 0, 'blank needle must not match everything');",
        ),
        (
            "label_dedupes",
            "<label for=i>Name</label><label for=i>Name again</label><input id=i>",
            "var r = B.findElements({label:'name'}); T(r.length === 1, J(r));",
        ),
        (
            "label_respects_hidden",
            "<label for=h>Secret</label><input id=h style='display:none'>",
            "T(B.findElements({label:'secret'}).length === 0, 'hidden'); \
             var r = B.findElements({label:'secret', include_hidden:true}); \
             T(r.length === 1 && r[0].visible === false, J(r));",
        ),
    ]);
}

#[test]
fn select_option_with_no_values_clears_only_a_multi_select() {
    check(&[
        (
            "multiple_clears",
            "<select multiple id=s><option value=a selected>A<option value=b selected>B</select>",
            "var n = 0; $('#s').addEventListener('change', function(){n++}); \
             var r = await B.selectOption(refOf('#s'), [], 200); T(r.ok, J(r)); \
             T($('#s').selectedOptions.length === 0, 'cleared'); T(n === 1, 'change fired');",
        ),
        (
            "single_still_refuses",
            "<select id=s><option value=a>A<option value=b>B</select>",
            "var r = await B.selectOption(refOf('#s'), [], 200); T(r.ok === false, J(r));",
        ),
    ]);
}

#[test]
fn a_transparent_subtree_is_marked_all_the_way_down() {
    check(&[(
        "children",
        "<div style='opacity:0'><section><button>in</button></section></div><button>out</button>",
        "var t = B.snapshot().tree.split('\\n'); \
         var inn = t.filter(function(l){return l.indexOf('\"in\"') !== -1})[0]; \
         var out = t.filter(function(l){return l.indexOf('\"out\"') !== -1})[0]; \
         T(inn.indexOf('[transparent]') !== -1, inn); T(out.indexOf('[transparent]') === -1, out); \
         var buttons = []; (function walk(n){ if (n.tag === 'button') buttons.push(n); (n.children||[]).forEach(walk); })(B.snapshot('json').tree); \
         T(buttons[0].transparent === true, J(buttons[0])); T(!buttons[1].transparent, J(buttons[1]));",
    )]);
}

/// The log projections used to read "no bridge" as "nothing happened": `?.get() || []`.
#[test]
fn log_projections_refuse_a_page_with_no_bridge() {
    use victauri_plugin::mcp::ipc_log_js::{
        ghost_ipc_outcomes_js, ipc_catalog_projection_js, ipc_integrity_js,
        ipc_timing_projection_js, slow_ipc_js, trimmed_log_js,
    };
    let build = |name: &str, code: String| {
        (
            name.to_string(),
            format!(
                "var threw = null; try {{ new Function('window', {src})({{}}); }} catch (e) {{ threw = e.message; }} \
                 T(threw === 'the page has no Victauri bridge', 'got: ' + threw);",
                src = serde_json::to_string(&code).unwrap()
            ),
        )
    };
    let cases = [
        build(
            "trimmed",
            trimmed_log_js("window.__VICTAURI__?.getIpcLog(5)", 5),
        ),
        build("integrity", ipc_integrity_js(1000)),
        build("slow", slow_ipc_js(10, 5)),
        build("ghost", ghost_ipc_outcomes_js(None)),
        build("timing", ipc_timing_projection_js(None)),
        build("catalog", ipc_catalog_projection_js().to_string()),
    ];
    let cases: Vec<(&str, &str, &str)> = cases
        .iter()
        .map(|(n, c)| (n.as_str(), "<div></div>", c.as_str()))
        .collect();
    check(&cases);
}

/// `ipc_was_called_with` must be able to tell "called with other args" from "args not in the log".
#[test]
fn ipc_log_entries_say_when_args_are_not_complete() {
    use victauri_plugin::mcp::ipc_log_js::trimmed_log_js;
    let trim = trimmed_log_js("window.__VICTAURI__.getIpcLog(0)", 100);
    let code = format!(
        r"
        function call(cmd, body) {{ return fetch('http://ipc.localhost/' + cmd, {{ method: 'POST', body: body }}).catch(function() {{}}); }}
        await call('small', JSON.stringify({{ a: 1 }}));
        await call('none', '{{}}');
        await call('trimmed', JSON.stringify({{ s: 'x'.repeat(5000) }}));
        await call('huge', JSON.stringify({{ s: 'x'.repeat(70000) }}));
        await call('binary', new Uint8Array(3));
        var by = {{}};
        B.getIpcLog().forEach(function(e) {{ by[e.command] = e; }});
        T(by.small.args.a === 1 && by.small.args_truncated === undefined, J(by.small));
        T(by.none.args_truncated === undefined, J(by.none));
        T(by.trimmed.args_truncated === undefined, 'whole in the bridge');
        T(by.huge.args_truncated === true, 'over the capture cap');
        T(J(by.binary.args) === '{{}}' && by.binary.args_truncated === true, J(by.binary));
        var view = {{}};
        (function() {{ {trim} }})().forEach(function(e) {{ view[e.command] = e; }});
        T(view.small.args.a === 1 && view.small.args_truncated === undefined, J(view.small));
        T(view.trimmed.args_truncated === true && typeof view.trimmed.args === 'string', J(view.trimmed).slice(0, 200));
        var light = B.getIpcLog(0, {{ bodies: false }});
        T(light.every(function(e) {{ return e.args === undefined && e.args_truncated === undefined; }}), 'bodies:false');
        "
    );
    check(&[("args", "<div></div>", &code)]);
}
