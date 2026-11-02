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

    /// A real click into a scaled, rotated or zoomed frame lands elsewhere: the walk adds the
    /// frame's offset and nothing can invert the rest, so it hit another live control. Refused.
    /// jsdom computes no style, so each case states what an engine computes (`matrix(...)`).
    #[test]
    fn click_probe_refuses_a_frame_it_cannot_place_a_click_in() {
        let probe = trusted_click_probe_js("__VREF__");
        let frame = |frame_style: &str, ancestor_style: &str| {
            format!(
                "var host = document.getElementById('frame-host'); \
                 var f = document.createElement('iframe'); host.appendChild(f); \
                 var fw = f.contentWindow, fd = f.contentDocument; \
                 fd.body.innerHTML = '<button id=\"inner\">In frame</button>'; \
                 var baseRect = window.HTMLElement.prototype.getBoundingClientRect; \
                 fw.HTMLElement.prototype.getBoundingClientRect = baseRect; \
                 fw.HTMLElement.prototype.scrollIntoView = function () {{}}; \
                 window.HTMLElement.prototype.getBoundingClientRect = function () {{ \
                   if (this.tagName === 'IFRAME') return {{left: 100, top: 200, width: 300, \
                     height: 150, right: 400, bottom: 350, x: 100, y: 200}}; \
                   return baseRect.call(this); }}; \
                 fd.elementFromPoint = function () {{ return fd.getElementById('inner'); }}; \
                 document.elementFromPoint = function () {{ return f; }}; \
                 var cs = window.getComputedStyle; \
                 window.getComputedStyle = function (el) {{ var o = cs.call(window, el), add = \
                   el === f ? {frame_style} : (el === host ? {ancestor_style} : {{}}); \
                   return new Proxy(o, {{ get: function (t, k) {{ return k in add ? add[k] : t[k]; }} }}); }};"
            )
        };
        let find = "{tag:'button', text:'In frame'}";
        let refused: Vec<(&str, String)> = vec![
            (
                "scale",
                frame("{transform:'matrix(2, 0, 0, 2, 0, 0)'}", "{}"),
            ),
            (
                "rotate",
                frame("{transform:'matrix(0, 1, -1, 0, 0, 0)'}", "{}"),
            ),
            (
                "3d scale",
                frame(
                    "{transform:'matrix3d(2,0,0,0,0,2,0,0,0,0,1,0,0,0,0,1)'}",
                    "{}",
                ),
            ),
            ("scale property", frame("{scale:'2'}", "{}")),
            ("rotate property", frame("{rotate:'45deg'}", "{}")),
            ("zoom", frame("{zoom:'2'}", "{}")),
            (
                "ancestor scale",
                frame("{}", "{transform:'matrix(1.5, 0, 0, 1.5, 0, 0)'}"),
            ),
            ("ancestor zoom", frame("{}", "{zoom:'1.5'}")),
        ];
        let allowed: Vec<(&str, String)> = vec![
            ("no transform", frame("{}", "{}")),
            (
                "identity",
                frame(
                    "{transform:'matrix(1, 0, 0, 1, 0, 0)',scale:'none',rotate:'none',translate:'none',zoom:'1'}",
                    "{}",
                ),
            ),
            (
                "translation",
                frame("{transform:'matrix(1, 0, 0, 1, 30, 20)'}", "{}"),
            ),
            (
                "3d translation",
                frame(
                    "{transform:'matrix3d(1,0,0,0,0,1,0,0,0,0,1,0,30,20,0,1)'}",
                    "{}",
                ),
            ),
            ("translate property", frame("{translate:'30px 20px'}", "{}")),
            (
                "ancestor translation",
                frame("{}", "{transform:'matrix(1, 0, 0, 1, 5, 5)'}"),
            ),
        ];
        let no_frame = "var host = document.getElementById('frame-host'); host.appendChild(document.getElementById('btn')); \
             var cs = window.getComputedStyle; \
             window.getComputedStyle = function (el) { var o = cs.call(window, el); \
               return new Proxy(o, { get: function (t, k) { return (el === host && k === 'transform') ? 'matrix(2, 0, 0, 2, 0, 0)' : t[k]; } }); };".to_string();
        let no_frame = format!("{HIT_TEST} {no_frame}");
        let pinch = format!(
            "{HIT_TEST} window.visualViewport = {{scale: 2, offsetLeft: 0, offsetTop: 0}};"
        );
        let no_pinch = format!(
            "{HIT_TEST} window.visualViewport = {{scale: 1, offsetLeft: 0, offsetTop: 0}};"
        );
        let mut cases: Vec<(&str, &str, &str, &str, String)> = Vec::new();
        for (name, setup) in refused.iter().chain(allowed.iter()) {
            cases.push((name, PAGE, setup.as_str(), find, probe.clone()));
        }
        cases.push((
            "a scaled ancestor with no frame",
            PAGE,
            no_frame.as_str(),
            "{css:'#btn'}",
            probe.clone(),
        ));
        cases.push((
            "pinch zoom",
            PAGE,
            pinch.as_str(),
            "{css:'#btn'}",
            probe.clone(),
        ));
        cases.push((
            "no pinch zoom",
            PAGE,
            no_pinch.as_str(),
            "{css:'#btn'}",
            probe.clone(),
        ));
        let Some(results) = run(&cases) else {
            return;
        };
        for (i, (name, _)) in refused.iter().enumerate() {
            let why = results[i]["error"].as_str().unwrap_or_default();
            assert!(why.contains("synthetic"), "{name}: {}", results[i]);
        }
        for (i, (name, _)) in allowed.iter().enumerate() {
            let at = refused.len() + i;
            assert!(results[at]["x"].is_number(), "{name}: {}", results[at]);
        }
        let at = refused.len() + allowed.len();
        assert!(results[at]["x"].is_number(), "no frame: {}", results[at]);
        let why = results[at + 1]["error"].as_str().unwrap_or_default();
        assert!(why.contains("synthetic"), "pinch: {}", results[at + 1]);
        assert!(
            results[at + 2]["x"].is_number(),
            "no pinch: {}",
            results[at + 2]
        );
    }

    /// The trusted coordinates are the point the bridge found, not the box's centre: a real OS
    /// click at the centre of a stroke-only circle lands in empty space.
    #[test]
    fn click_probe_returns_the_point_that_was_found() {
        let probe = trusted_click_probe_js("__VREF__");
        // A circle of radius 40 around (100, 100), placed at (20, 10) by its screen CTM.
        let stroke = "window.Element.prototype.scrollIntoView = function () {}; \
             var c = document.getElementById('c'); \
             c.getBoundingClientRect = function () { return {left: 80, top: 70, width: 80, height: 80, \
               right: 160, bottom: 150, x: 80, y: 70}; }; \
             c.getTotalLength = function () { return 2 * Math.PI * 40; }; \
             c.getPointAtLength = function (s) { var t = s / 40; \
               return {x: 100 + 40 * Math.cos(t), y: 100 + 40 * Math.sin(t)}; }; \
             c.getScreenCTM = function () { return {a: 1, b: 0, c: 0, d: 1, e: 20, f: 10}; }; \
             window.__onStroke = function (x, y) { return Math.abs(Math.hypot(x - 120, y - 110) - 40) < 1; }; \
             document.elementFromPoint = function (x, y) { \
               return window.__onStroke(x, y) ? c : document.body; };";
        let stroke_html = r#"<html><body><svg width="200" height="200"><circle id="c" r="40"></circle></svg></body></html>"#;
        let stroke_framed = "var f = document.createElement('iframe'); document.body.appendChild(f); \
             var fw = f.contentWindow, fd = f.contentDocument; \
             fd.body.innerHTML = '<svg width=\"200\" height=\"200\"><circle id=\"c\" r=\"40\"></circle></svg>'; \
             fw.Element.prototype.scrollIntoView = function () {}; \
             var c = fd.getElementById('c'); \
             c.getBoundingClientRect = function () { return {left: 80, top: 70, width: 80, height: 80, \
               right: 160, bottom: 150, x: 80, y: 70}; }; \
             c.getTotalLength = function () { return 2 * Math.PI * 40; }; \
             c.getPointAtLength = function (s) { var t = s / 40; \
               return {x: 100 + 40 * Math.cos(t), y: 100 + 40 * Math.sin(t)}; }; \
             c.getScreenCTM = function () { return {a: 1, b: 0, c: 0, d: 1, e: 20, f: 10}; }; \
             fd.elementFromPoint = function (x, y) { \
               return Math.abs(Math.hypot(x - 120, y - 110) - 40) < 1 ? c : fd.body; }; \
             f.getBoundingClientRect = function () { return {left: 1000, top: 2000, width: 300, height: 300, \
               right: 1300, bottom: 2300, x: 1000, y: 2000}; }; \
             window.innerWidth = 5000; window.innerHeight = 5000; \
             document.elementFromPoint = function () { return f; };";
        let wrapped = "var l = document.getElementById('l'); \
             l.getBoundingClientRect = function () { return {left: 0, top: 0, width: 100, height: 60, \
               right: 100, bottom: 60, x: 0, y: 0}; }; \
             l.getClientRects = function () { return [ \
               {left: 80, top: 0, right: 100, bottom: 16, width: 20, height: 16}, \
               {left: 0, top: 44, right: 30, bottom: 60, width: 30, height: 16}]; }; \
             document.elementFromPoint = function (x, y) { \
               return (Math.floor(x) === 15 && Math.floor(y) === 52) ? l : document.body; };";
        let wrapped_html = r#"<html><body><a id="l" href="x">wrapped</a></body></html>"#;
        let clipped_away = "window.Element.prototype.scrollIntoView = function () {}; \
             document.getElementById('c').getBoundingClientRect = function () { \
               return {left: 80, top: 70, width: 80, height: 80, right: 160, bottom: 150, x: 80, y: 70}; }; \
             document.elementFromPoint = function () { return document.body; };";
        let Some(results) = run(&[
            (
                "stroke-only circle",
                stroke_html,
                stroke,
                "{css:'#c'}",
                probe.clone(),
            ),
            (
                "stroke-only circle in a frame",
                "<html><body></body></html>",
                stroke_framed,
                "{css:'#c'}",
                probe.clone(),
            ),
            (
                "second line of a wrapped link",
                wrapped_html,
                wrapped,
                "{css:'#l'}",
                probe.clone(),
            ),
            (
                "nothing of it can be hit",
                stroke_html,
                clipped_away,
                "{css:'#c'}",
                probe,
            ),
        ]) else {
            return;
        };
        let on_stroke = |v: &serde_json::Value, ox: f64, oy: f64| {
            let (x, y) = (v["x"].as_f64().unwrap(), v["y"].as_f64().unwrap());
            ((x - ox - 120.0).hypot(y - oy - 110.0) - 40.0).abs() < 1.0
        };
        assert!(on_stroke(&results[0], 0.0, 0.0), "{}", results[0]);
        assert!(on_stroke(&results[1], 1000.0, 2000.0), "{}", results[1]);
        assert_eq!(results[2]["x"], 15.0, "{}", results[2]);
        assert_eq!(results[2]["y"], 52.0, "{}", results[2]);
        assert!(
            results[3]["error"].as_str().unwrap().contains("covered")
                && results[3].get("x").is_none(),
            "{}",
            results[3]
        );
    }

    /// A button inside an OPEN shadow root, hit-tested the way a browser does:
    /// `document.elementFromPoint` reports the shadow HOST (retargeting); only the shadow root's
    /// own `elementFromPoint` sees the button. `cover_inner` makes something else inside the
    /// shadow tree the hit element instead.
    fn shadow_setup(cover_inner: bool) -> String {
        format!(
            "var h = document.createElement('div'); h.id = 'host'; document.body.appendChild(h); \
             var sr = h.attachShadow({{mode: 'open'}}); \
             sr.innerHTML = '<button id=\"sb\">Shadow Go</button><span id=\"sc\">cover</span>'; \
             document.elementFromPoint = function () {{ return h; }}; \
             sr.elementFromPoint = function () {{ return sr.getElementById('{}'); }};",
            if cover_inner { "sc" } else { "sb" }
        )
    }

    /// Round 4 (regression review): `elementFromPoint` retargets to the shadow host and
    /// `Node.contains` never crosses a shadow boundary, so EVERY element inside a web component
    /// read as "covered by <host>" — refused for trusted clicks and, since the actionability
    /// check was added, for synthetic `interact click` too (reproduced live on `WebView2`).
    #[test]
    fn clicks_reach_elements_inside_open_shadow_roots() {
        let probe = trusted_click_probe_js("__VREF__");
        let synthetic = "return await window.__VICTAURI__.click(\"__VREF__\")".to_string();
        let open = shadow_setup(false);
        let covered = shadow_setup(true);
        let find = "{text:'Shadow Go'}";
        let Some(results) = run(&[
            ("trusted: shadow button", PAGE, &open, find, probe.clone()),
            (
                "trusted: covered inside the shadow tree",
                PAGE,
                &covered,
                find,
                probe,
            ),
            (
                "synthetic: shadow button",
                PAGE,
                &open,
                find,
                synthetic.clone(),
            ),
            (
                "synthetic: covered inside the shadow tree",
                PAGE,
                &covered,
                find,
                synthetic,
            ),
        ]) else {
            return;
        };
        assert!(
            results[0]["x"].is_number(),
            "trusted shadow click: {}",
            results[0]
        );
        assert!(
            results[1]["error"].is_string(),
            "trusted covered: {}",
            results[1]
        );
        assert_eq!(
            results[2]["ok"], true,
            "synthetic shadow click: {}",
            results[2]
        );
        assert_eq!(results[3]["ok"], false, "synthetic covered: {}", results[3]);
    }
}
