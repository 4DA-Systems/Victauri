//! Round-5 tool-result regressions (R5B-*): expression wrapping, `undefined` in assertions,
//! `wait_for` edge cases, `logs slow_ipc` window targeting, `explain diff` accounting, and
//! page-reported failures surfacing as tool errors. Each drives the real `execute_tool`
//! dispatch with a bridge that answers the page's side of the protocol.

use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use serde_json::json;
use victauri_core::WindowState;

use super::*;

/// What the page answers for one eval wrapper: `(target label, wrapper script) -> callback body`
/// (`None` leaves the eval unanswered).
type Responder = Arc<dyn Fn(Option<&str>, &str) -> Option<String> + Send + Sync>;

/// A bridge that answers the liveness probe, and every eval wrapper through a [`Responder`].
#[derive(Clone)]
struct ScriptBridge {
    pending: crate::PendingCallbacks,
    respond: Responder,
    /// `(label, script)` of every eval delivered.
    evals: Arc<StdMutex<Vec<(Option<String>, String)>>>,
}

impl ScriptBridge {
    fn new(state: &Arc<VictauriState>, respond: Responder) -> Self {
        Self {
            pending: state.pending_evals.clone(),
            respond,
            evals: Arc::default(),
        }
    }

    /// A bridge that answers every eval with the same callback body.
    fn answering(state: &Arc<VictauriState>, body: &str) -> Self {
        let body = body.to_string();
        Self::new(state, Arc::new(move |_, _| Some(body.clone())))
    }

    fn answer(&self, id: String, body: String) {
        let pending = self.pending.clone();
        std::thread::spawn(move || {
            if let Some(tx) = pending.blocking_lock().remove(&id) {
                let _ = tx.send(body);
            }
        });
    }

    /// Labels of every eval whose script contains `needle`.
    fn labels_of(&self, needle: &str) -> Vec<Option<String>> {
        self.evals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(_, s)| s.contains(needle))
            .map(|(l, _)| l.clone())
            .collect()
    }
}

/// The quoted 36-char id right after `marker`.
fn quoted_id_after(script: &str, marker: &str) -> Option<String> {
    let start = script.find(marker)? + marker.len();
    script.get(start..start + 36).map(str::to_string)
}

impl WebviewBridge for ScriptBridge {
    fn eval_webview(&self, label: Option<&str>, script: &str) -> Result<(), String> {
        self.evals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((label.map(str::to_string), script.to_string()));
        if script.contains("probe_ok") {
            if let Some(id) = quoted_id_after(script, "id:\"") {
                self.answer(id, "\"probe_ok:page-1\"".to_string());
            }
        } else if let Some(id) = quoted_id_after(script, "__vic = { id: \"")
            && let Some(body) = (self.respond)(label, script)
        {
            self.answer(id, body);
        }
        Ok(())
    }
    fn get_window_states(&self, _l: Option<&str>) -> Vec<WindowState> {
        Vec::new()
    }
    fn list_window_labels(&self) -> Vec<String> {
        vec!["main".to_string(), "popup".to_string()]
    }
    fn get_native_handle(&self, _l: Option<&str>) -> Result<isize, String> {
        Err("no handle".to_string())
    }
    fn manage_window(&self, _l: Option<&str>, _a: &str) -> Result<String, String> {
        Err("no window".to_string())
    }
    fn resize_window(&self, _l: Option<&str>, _w: u32, _h: u32) -> Result<(), String> {
        Ok(())
    }
    fn move_window(&self, _l: Option<&str>, _x: i32, _y: i32) -> Result<(), String> {
        Ok(())
    }
    fn set_window_title(&self, _l: Option<&str>, _t: &str) -> Result<(), String> {
        Ok(())
    }
}

fn state() -> Arc<VictauriState> {
    let mut s = VictauriState::for_tests();
    s.eval_timeout = Duration::from_secs(5);
    Arc::new(s)
}

fn handler(state: &Arc<VictauriState>, bridge: &ScriptBridge) -> VictauriMcpHandler {
    VictauriMcpHandler::new(Arc::clone(state), Arc::new(bridge.clone()))
}

async fn call(h: &VictauriMcpHandler, tool: &str, args: serde_json::Value) -> CallToolResult {
    tokio::time::timeout(Duration::from_secs(30), h.execute_tool(tool, args))
        .await
        .unwrap_or_else(|_| panic!("{tool} did not return within 30s"))
        .unwrap_or_else(|_| panic!("{tool}: dispatch rejected the arguments"))
}

fn text(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| match c {
            ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn ok_json(r: &CallToolResult) -> serde_json::Value {
    assert_ne!(r.is_error, Some(true), "unexpected tool error: {}", text(r));
    serde_json::from_str(&text(r)).unwrap_or_else(|e| panic!("not JSON ({e}): {}", text(r)))
}

// ── R5B-EXPR1: expression wrapping ─────────────────────────────────────────────────────────

/// The code the eval engine finally runs for `code` — the same `return` auto-prepend as
/// `eval_outcome`.
fn engine_code(code: &str) -> String {
    let body = strip_leading_js_comments(code.trim());
    if should_prepend_return(body) {
        format!("return {body}")
    } else {
        code.trim().to_string()
    }
}

/// Every expression in `cases` runs (in Node, inside the same async-function body the eval
/// wrapper uses) and yields 2; the invalid one is a syntax error.
#[test]
fn expression_tools_accept_trailing_semicolons_and_comments() {
    let cases = [
        "1+1",
        "1+1;",
        "1+1 // note",
        "1+1;  // note",
        "1+1; /* note */",
        "1+1;;  ",
        "1+1 /* a */ // b",
        "/* lead */ 1+1",
        "// lead\n1+1",
        "1 +\n1",
        "(1\n+ 1)",
        "[1,\n 1]\n  .length",
        "  \n 1+1 \n ",
        "'a//b'.length - 2",
        "\"x;\".length",
        "{a: 2}.a",
        "({a: 2}).a;",
        "await Promise.resolve(2)",
        "await Promise.resolve(2); // done",
    ];
    let bodies: Vec<String> = cases
        .iter()
        .map(|c| engine_code(&expression_eval_code(c)))
        .collect();
    let invalid = engine_code(&expression_eval_code("1 +"));
    let script = format!(
        "const AF = Object.getPrototypeOf(async function(){{}}).constructor;
         (async () => {{
           const out = [];
           for (const body of {bodies}) {{
             try {{ out.push(await new AF(body + '\\n')()); }}
             catch (e) {{ out.push('ERR ' + e.name + ': ' + e.message); }}
           }}
           let invalid;
           try {{ new AF({invalid} + '\\n'); invalid = 'parsed'; }}
           catch (e) {{ invalid = e.name; }}
           console.log('OUT:' + JSON.stringify({{ out, invalid }}));
         }})();",
        bodies = serde_json::to_string(&bodies).unwrap(),
        invalid = serde_json::to_string(&invalid).unwrap(),
    );
    let Ok(out) = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
    else {
        eprintln!("SKIP: node not installed");
        return;
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find_map(|l| l.strip_prefix("OUT:"))
        .unwrap_or_else(|| {
            panic!(
                "no output: {stdout} {}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    for (i, case) in cases.iter().enumerate() {
        assert_eq!(
            v["out"][i],
            json!(2),
            "{case:?} -> {:?}: {}",
            bodies[i],
            v["out"][i]
        );
    }
    assert_eq!(v["invalid"], "SyntaxError", "{invalid:?}");
}

/// A genuine syntax error in a `wait_for` expression fails at once, not after polling to the
/// timeout (it can never start matching).
#[tokio::test]
async fn wait_for_expression_with_a_syntax_error_fails_fast() {
    let state = state();
    let bridge = ScriptBridge::answering(
        &state,
        r#"{"__victauri_not_run":"the code did not begin executing — this almost always means a syntax/parse error in the submitted code"}"#,
    );
    let h = handler(&state, &bridge);
    let started = Instant::now();
    let r = call(
        &h,
        "wait_for",
        json!({"condition": "expression", "value": "1 +", "timeout_ms": 10_000}),
    )
    .await;
    assert_eq!(r.is_error, Some(true), "{}", text(&r));
    assert!(text(&r).contains("parse error"), "{}", text(&r));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "polled for {:?}",
        started.elapsed()
    );
}

/// A page-side exception (the target may not exist yet during startup) is still "not yet
/// met": `wait_for` keeps polling and surfaces it on timeout.
#[tokio::test]
async fn wait_for_expression_keeps_polling_through_a_page_exception() {
    let state = state();
    let bridge = ScriptBridge::answering(&state, r#"{"__victauri_err":"x is not defined"}"#);
    let h = handler(&state, &bridge);
    let r = ok_json(
        &call(
            &h,
            "wait_for",
            json!({"condition": "expression", "value": "x.ready", "timeout_ms": 300,
                   "poll_ms": 50}),
        )
        .await,
    );
    assert_eq!(r["ok"], false, "{r}");
    assert!(
        r["last_error"]
            .as_str()
            .is_some_and(|e| e.contains("x is not defined")),
        "{r}"
    );
    assert!(bridge.labels_of("x.ready").len() > 1, "did not poll");
}

// ── R5B-ASSERT1: `undefined` is a value assertions can test ────────────────────────────────

const UNDEFINED: &str = r#"{"__victauri_ok":null,"__victauri_type":"undefined"}"#;

/// An expression that evaluates to `undefined` (a missing property, an absent global) used to
/// fail with "not valid JSON", so `exists` / `falsy` could not be asserted against it. It is
/// now read as `null` — what JSON makes of it, and what JavaScript's `x == null` says.
#[tokio::test]
async fn assert_semantic_treats_undefined_as_null() {
    let state = state();
    let bridge = ScriptBridge::answering(&state, UNDEFINED);
    let h = handler(&state, &bridge);
    for (condition, expected, passes) in [
        ("exists", json!(null), false),
        ("falsy", json!(null), true),
        ("truthy", json!(null), false),
        ("equals", json!(null), true),
        ("not_equals", json!(null), false),
        ("type_is", json!("null"), true),
    ] {
        let r = ok_json(
            &call(
                &h,
                "assert_semantic",
                json!({"expression": "window.nothingHere", "condition": condition,
                       "expected": expected}),
            )
            .await,
        );
        assert_eq!(r["passed"], passes, "{condition}: {r}");
        assert_eq!(r["actual"], json!(null), "{condition}: {r}");
    }
}

/// `verify_state` reads an `undefined` frontend value the same way: it is compared (as `null`)
/// instead of failing the call.
#[tokio::test]
async fn verify_state_treats_undefined_as_null() {
    let state = state();
    let bridge = ScriptBridge::answering(&state, UNDEFINED);
    let h = handler(&state, &bridge);
    let r = ok_json(
        &call(
            &h,
            "verify_state",
            json!({"frontend_expr": "window.nothingHere", "backend_state": {"a": 1}}),
        )
        .await,
    );
    assert_eq!(r["passed"], false, "{r}");
    assert_eq!(r["frontend_state"], json!(null), "{r}");
}

// ── R5B-WAIT0: `timeout_ms: 0` is a single immediate check ─────────────────────────────────

/// The server-side conditions check exactly once with `timeout_ms: 0` (the page-side ones are
/// covered by `tests/bridge_r5b_tests.rs`), and the page-side wait is given time to answer.
#[tokio::test]
async fn wait_for_with_a_zero_timeout_checks_once() {
    let state = state();
    let bridge = ScriptBridge::new(
        &state,
        Arc::new(|_, script: &str| {
            Some(if script.contains("waitFor(") {
                json!({"__victauri_ok": {"ok": false, "error": "timeout after 0ms"},
                       "__victauri_type": "value"})
                .to_string()
            } else {
                json!({"__victauri_ok": script.contains("isReady"), "__victauri_type": "value"})
                    .to_string()
            })
        }),
    );
    let h = handler(&state, &bridge);
    let started = Instant::now();
    let met = ok_json(
        &call(
            &h,
            "wait_for",
            json!({"condition": "expression", "value": "isReady", "timeout_ms": 0}),
        )
        .await,
    );
    assert_eq!(met["ok"], true, "{met}");
    let unmet = ok_json(
        &call(
            &h,
            "wait_for",
            json!({"condition": "expression", "value": "notYet", "timeout_ms": 0}),
        )
        .await,
    );
    assert_eq!(unmet["ok"], false, "{unmet}");
    assert_eq!(
        bridge.labels_of("notYet").len(),
        1,
        "must check exactly once"
    );
    let event = ok_json(
        &call(
            &h,
            "wait_for",
            json!({"condition": "event", "value": "never", "timeout_ms": 0}),
        )
        .await,
    );
    assert_eq!(event["ok"], false, "{event}");
    let page = ok_json(
        &call(
            &h,
            "wait_for",
            json!({"condition": "selector", "value": "#x", "timeout_ms": 0}),
        )
        .await,
    );
    assert_eq!(page["ok"], false, "{page}");
    assert!(
        bridge
            .evals
            .lock()
            .unwrap()
            .iter()
            .any(|(_, s)| s.contains("timeout_ms: 0,")),
        "the page must be told 0, not a default"
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}
