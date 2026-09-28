//! The trusted-input probes (R4-IN1 / R4-IN2) run in a real JS engine (jsdom, via the bridge
//! test runner in `tests/bridge_tests/`) against the real bridge script: what they report
//! decides whether OS-level input is sent at all, so their page-side logic is what these
//! tests pin.
//!
//! They live in their own test binary, not the library's unit tests: a child process spawned
//! on Windows inherits the sockets open in the spawning process at that moment, and a
//! seconds-long `node` run then holds another test's server connection open past its close.

mod trusted_probe_js {
    use std::io::Write;
    use std::path::PathBuf;
    use victauri_plugin::mcp::{trusted_click_probe_js, trusted_focus_probe_js};

    fn runner_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("bridge_tests")
    }

    /// One case: (name, page html, setup js, `findElements` query for the target, probe).
    type Case<'a> = (&'a str, &'a str, &'a str, &'a str, String);

    /// Each probe is built for the placeholder ref `__VREF__` and run after resolving the
    /// real ref of the first element the case's query matches. Returns each case's result,
    /// or `None` when jsdom is not installed (and nothing requires it).
    fn run(cases: &[Case<'_>]) -> Option<Vec<serde_json::Value>> {
        if !runner_dir().join("node_modules").join("jsdom").exists() {
            assert!(
                std::env::var_os("CI").is_none()
                    && std::env::var_os("VICTAURI_REQUIRE_JSDOM").is_none(),
                "jsdom is not installed: `npm ci` in crates/victauri-plugin/tests/bridge_tests/"
            );
            eprintln!("SKIP: jsdom not installed");
            return None;
        }
        let tests: Vec<serde_json::Value> = cases
            .iter()
            .map(|(name, html, setup_js, find_query, probe)| {
                let code = format!(
                    "var __found = window.__VICTAURI__.findElements({find_query}); \
                     if (!__found.length) throw new Error('fixture element not found'); \
                     var __vref = __found[0].ref_id;\n{}",
                    probe.replace("\"__VREF__\"", "__vref")
                );
                serde_json::json!({
                    "name": name, "code": code,
                    "setup_html": html, "setup_js": setup_js,
                })
            })
            .collect();
        let def = serde_json::json!({
            "bridge_script": victauri_plugin::js_bridge::init_script(
                &victauri_plugin::js_bridge::BridgeCapacities::default()
            ),
            "setup_html": "<html><body></body></html>",
            "tests": tests,
        });
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(def.to_string().as_bytes()).unwrap();
        tmp.flush().unwrap();
        let out = match std::process::Command::new("node")
            .arg(runner_dir().join("run_tests.js"))
            .arg(tmp.path())
            .output()
        {
            Ok(out) => out,
            Err(e) => {
                assert!(
                    std::env::var_os("CI").is_none()
                        && std::env::var_os("VICTAURI_REQUIRE_JSDOM").is_none(),
                    "node could not be run: {e}"
                );
                eprintln!("SKIP: node could not be run: {e}");
                return None;
            }
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout
            .lines()
            .find_map(|l| l.strip_prefix("VICTAURI_RESULTS:"))
            .unwrap_or_else(|| {
                panic!(
                    "no results: {stdout}\n{}",
                    String::from_utf8_lossy(&out.stderr)
                )
            });
        let results: Vec<serde_json::Value> = serde_json::from_str(line).unwrap();
        Some(
            results
                .into_iter()
                .map(|r| {
                    assert_eq!(r["passed"], true, "{}: {}", r["name"], r["error"]);
                    r["result"].clone()
                })
                .collect(),
        )
    }

    const FORM: &str = r#"<html><body>
        <input id="target" placeholder="target">
        <input id="other" placeholder="other">
        <div id="plain">not focusable</div>
        <div id="host"></div>
    </body></html>"#;

    /// R4-IN1: the focus probe must report whether focus LANDED on the element, not
    /// whether the element exists — keys go to whatever holds focus.
    #[test]
    fn focus_probe_reports_where_focus_actually_landed() {
        let probe = trusted_focus_probe_js("__VREF__");
        let shadow_setup = "var r = document.getElementById('host').attachShadow({mode:'open'}); \
                            r.innerHTML = '<input placeholder=\"in-shadow\">';";
        let Some(results) = run(&[
            (
                "plain input",
                FORM,
                "",
                "{placeholder:'target'}",
                probe.clone(),
            ),
            (
                "a focus handler moves focus elsewhere",
                FORM,
                "document.getElementById('target').addEventListener('focus', function () { \
                   document.getElementById('other').focus(); });",
                "{placeholder:'target'}",
                probe.clone(),
            ),
            (
                "element that cannot take focus",
                FORM,
                "",
                "{css:'#plain'}",
                probe.clone(),
            ),
            (
                "input inside an open shadow root",
                FORM,
                shadow_setup,
                "{placeholder:'in-shadow'}",
                probe.clone(),
            ),
        ]) else {
            return;
        };
        let focused = |v: &serde_json::Value| v["focused"] == true && v["found"] == true;
        assert!(focused(&results[0]), "plain input: {}", results[0]);
        assert!(!focused(&results[1]), "focus moved away: {}", results[1]);
        assert_eq!(results[1]["found"], true, "{}", results[1]);
        assert!(!focused(&results[2]), "not focusable: {}", results[2]);
        assert!(focused(&results[3]), "shadow input: {}", results[3]);
    }

    const PAGE: &str = r#"<html><body>
        <button id="btn">Go</button>
        <button id="off" disabled>Off</button>
        <div id="cover">cover</div>
        <div id="frame-host"></div>
    </body></html>"#;

    /// A hit-test for jsdom (which has no layout): the element under any point is `#btn`,
    /// unless a case replaces `window.__hit`.
    const HIT_TEST: &str = "window.__hit = function (doc) { \
                            return doc.getElementById('btn') || doc.body; }; \
                            document.elementFromPoint = function (x, y) { \
                            return window.__hit(document, x, y); };";

    /// R4-IN2: the trusted-click probe takes coordinates from page-controlled layout, so it
    /// must refuse a point that is off-screen, covered or on a disabled element (the OS click
    /// would land on something else), and must add a same-origin frame's offset.
    #[test]
    fn click_probe_refuses_unclickable_points_and_offsets_frames() {
        let probe = trusted_click_probe_js("__VREF__");
        let off_screen = format!(
            "{HIT_TEST} var b = document.getElementById('btn'); \
             b.getBoundingClientRect = function () {{ return {{left: 5000, top: 10, \
             width: 80, height: 32, right: 5080, bottom: 42, x: 5000, y: 10}}; }};"
        );
        let covered = format!(
            "{HIT_TEST} window.__hit = function (doc) {{ return doc.getElementById('cover'); }};"
        );
        let in_frame = "var f = document.createElement('iframe'); \
             document.getElementById('frame-host').appendChild(f); \
             var fw = f.contentWindow, fd = f.contentDocument; \
             fd.body.innerHTML = '<button id=\"inner\">In frame</button>'; \
             var baseRect = window.HTMLElement.prototype.getBoundingClientRect; \
             fw.HTMLElement.prototype.getBoundingClientRect = baseRect; \
             fw.HTMLElement.prototype.scrollIntoView = function () {}; \
             window.HTMLElement.prototype.getBoundingClientRect = function () { \
               if (this.tagName === 'IFRAME') return {left: 100, top: 200, width: 300, \
                 height: 150, right: 400, bottom: 350, x: 100, y: 200}; \
               return baseRect.call(this); }; \
             fd.elementFromPoint = function () { return fd.getElementById('inner'); }; \
             document.elementFromPoint = function () { return f; };";
        let Some(results) = run(&[
            (
                "visible button",
                PAGE,
                HIT_TEST,
                "{css:'#btn'}",
                probe.clone(),
            ),
            (
                "button laid out off-screen",
                PAGE,
                &off_screen,
                "{css:'#btn'}",
                probe.clone(),
            ),
            (
                "button covered by another element",
                PAGE,
                &covered,
                "{css:'#btn'}",
                probe.clone(),
            ),
            (
                "disabled button",
                PAGE,
                HIT_TEST,
                "{css:'#off'}",
                probe.clone(),
            ),
            (
                "button in a same-origin iframe",
                PAGE,
                in_frame,
                "{tag:'button', text:'In frame'}",
                probe.clone(),
            ),
        ]) else {
            return;
        };
        // jsdom's stub lays every BUTTON out at (10,10) 80×32: center (50, 26).
        assert_eq!(results[0]["x"], 50.0, "{}", results[0]);
        assert_eq!(results[0]["y"], 26.0, "{}", results[0]);
        for (i, why) in [(1, "off-screen"), (2, "covered"), (3, "disabled")] {
            assert!(
                results[i]["error"].is_string() && results[i].get("x").is_none(),
                "{why}: {}",
                results[i]
            );
        }
        // Frame content origin (100, 200) + the button's center inside the frame (50, 26).
        assert_eq!(results[4]["x"], 150.0, "{}", results[4]);
        assert_eq!(results[4]["y"], 226.0, "{}", results[4]);
    }
}
