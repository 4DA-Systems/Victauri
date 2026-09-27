//! `victauri logs` — read (or follow) a Tauri app's backend logs from the terminal.
//!
//! Live app: reads the structured backend log through the plugin's REST API
//! (`logs backend` / `backend_digest` / `stdout`). App down (crashed, rebuilding,
//! not started): falls back to the newest `victauri run` console capture on disk,
//! whose last record says how the app ended.
//!
//! `--follow` prints new lines as they arrive, so the command can be handed to an
//! agent harness's line-streaming monitor, e.g.
//! `victauri logs --follow --level warn` → one notification per backend warning.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// Options for `victauri logs`.
#[allow(clippy::struct_excessive_bools)]
pub struct LogsOptions {
    pub app: Option<String>,
    pub level: Option<String>,
    pub target: Option<String>,
    pub grep: Option<String>,
    pub limit: usize,
    pub follow: bool,
    pub digest: bool,
    pub stdout: bool,
    pub json: bool,
    pub capture: Option<PathBuf>,
    pub interval_ms: u64,
}

struct Backend {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

impl Backend {
    async fn connect(app: Option<&str>) -> std::result::Result<(Self, String), String> {
        let (port, token, label) = crate::bridge::resolve_backend(app).await?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;
        Ok((
            Self {
                http,
                base: format!("http://127.0.0.1:{port}"),
                token,
            },
            label,
        ))
    }

    /// Poll `/health` for up to `max` — a busy app answers slowly, a dead one never.
    async fn recovers_within(&self, max: Duration) -> bool {
        let deadline = std::time::Instant::now() + max;
        while std::time::Instant::now() < deadline {
            let ok = self
                .http
                .get(format!("{}/health", self.base))
                .timeout(Duration::from_secs(2))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success());
            if ok {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        false
    }

    async fn logs(&self, args: Value) -> Result<Value> {
        let mut req = self
            .http
            .post(format!("{}/api/tools/logs", self.base))
            .json(&args);
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        let resp: Value = req
            .send()
            .await
            .context("app unreachable")?
            .json()
            .await
            .context("unexpected response")?;
        if let Some(err) = resp.get("error") {
            bail!(
                "{}",
                err.as_str().map_or_else(|| err.to_string(), str::to_string)
            );
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn level_rank(level: &str) -> u8 {
    match level.to_ascii_lowercase().as_str() {
        "trace" => 0,
        "debug" => 1,
        "info" => 2,
        "warn" | "warning" => 3,
        "error" => 4,
        _ => 0,
    }
}

/// Render one structured backend entry as a single terminal line.
#[must_use]
pub fn render_entry(e: &Value) -> String {
    let ts = e["ts_ms"].as_u64().unwrap_or(0);
    let (secs, ms) = (ts / 1000, ts % 1000);
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let level = e["level"].as_str().unwrap_or("?").to_ascii_uppercase();
    // One entry = one output line, always: a line-streaming consumer (a monitor that
    // turns each stdout line into an event) must never see a message split in two.
    let message = e["message"]
        .as_str()
        .unwrap_or("")
        .replace("\r\n", " \u{23ce} ")
        .replace('\n', " \u{23ce} ");
    let mut line = format!(
        "{h:02}:{m:02}:{s:02}.{ms:03}Z {level:<5} {}: {message}",
        e["target"].as_str().unwrap_or("")
    );
    if let Some(fields) = e["fields"].as_object() {
        for (k, v) in fields {
            if k == "backtrace" {
                continue;
            }
            match v {
                Value::String(s) => line.push_str(&format!(" {k}={s:?}")),
                other => line.push_str(&format!(" {k}={other}")),
            }
        }
    }
    if let Some(spans) = e["spans"].as_array().filter(|s| !s.is_empty()) {
        let path: Vec<&str> = spans.iter().filter_map(Value::as_str).collect();
        line.push_str(&format!("  [{}]", path.join(" > ")));
    }
    if e["source"] == "panic"
        && let Some(loc) = e["location"].as_str()
    {
        line.push_str(&format!("  at {loc}"));
    }
    line
}

fn render_console(l: &Value) -> String {
    let ts = l["t"].as_u64().unwrap_or(0);
    let (secs, ms) = (ts / 1000, ts % 1000);
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let stream = l["s"].as_str().unwrap_or("?");
    let tag = match stream {
        "err" => "ERR ",
        "exit" => "EXIT",
        _ => "OUT ",
    };
    let mut line = format!(
        "{h:02}:{m:02}:{s:02}.{ms:03}Z {tag} {}",
        l["l"].as_str().unwrap_or("")
    );
    if let Some(tail) = l["tail"].as_array().filter(|t| !t.is_empty()) {
        let last: Vec<&str> = tail.iter().filter_map(Value::as_str).collect();
        line.push_str(&format!(" | last stderr: {}", last.join(" \u{23ce} ")));
    }
    line
}

fn emit(line: &str, json: bool, raw: &Value) {
    if json {
        println!("{raw}");
    } else {
        println!("{line}");
    }
}

/// Entry point for `victauri logs`.
///
/// # Errors
/// Fails when neither a live app nor a console capture is available, or on a
/// tool error reported by the app.
pub async fn cmd_logs(opts: LogsOptions) -> Result<()> {
    if opts.stdout || opts.capture.is_some() {
        return follow_capture(&opts, opts.capture.clone(), None, None).await;
    }
    let (backend, label) = match Backend::connect(opts.app.as_deref()).await {
        Ok(b) => b,
        Err(why) if why.contains("several Victauri apps") => bail!("{why}"),
        Err(why) => {
            // App down: the console capture (if the app ran under `victauri run`) still
            // holds its output — including how it died.
            if crate::run::latest_capture().is_some() {
                eprintln!(
                    "victauri logs: {why} — showing the latest `victauri run` capture instead"
                );
                return follow_capture(&opts, None, None, None).await;
            }
            bail!(
                "{why}.\nStart the app (debug build), or launch it as `victauri run -- <dev command>` \
                 so its output is captured even while it is down."
            );
        }
    };
    eprintln!("victauri logs: {label}");

    if opts.digest {
        let d = backend
            .logs(json!({"action": "backend_digest", "limit": 10}))
            .await?;
        if opts.json {
            println!("{}", serde_json::to_string_pretty(&d)?);
        } else {
            print_digest(&d);
        }
        return Ok(());
    }

    let mut args = json!({
        "action": "backend",
        "limit": opts.limit,
        "level": opts.level,
        "target": opts.target,
        "filter": opts.grep,
    });
    let first = backend.logs(args.clone()).await?;
    let sources = &first["sources"];
    let structured = sources["tracing"] == true || sources["log"] == true;
    if !structured && sources["stdout_capture"] == true {
        eprintln!("victauri logs: no structured source in this app — reading its captured stdout");
        return follow_stdout_via_app(&backend, &opts).await;
    }
    if let Some(hint) = first["hint"].as_str() {
        eprintln!("victauri logs: {hint}");
    }
    // Where this app's raw output is captured (when run under `victauri run`): after a
    // crash that file still holds its last words and exit status.
    let capture_path = sources["stdout_capture_path"].as_str().map(PathBuf::from);
    // `-n 0` = no history, only what arrives from now on.
    if opts.limit > 0 {
        print_page(&first, &opts);
    }
    if !opts.follow {
        return Ok(());
    }
    let mut cursor = first["next_seq"].as_u64().unwrap_or(0);
    loop {
        tokio::time::sleep(Duration::from_millis(opts.interval_ms)).await;
        args["since_seq"] = cursor.into();
        args["limit"] = 2000.into();
        if let Ok(page) = backend.logs(args.clone()).await {
            if page["gap"] == true {
                eprintln!("victauri logs: [some entries were evicted before they could be read]");
            }
            print_page(&page, &opts);
            cursor = page["next_seq"].as_u64().unwrap_or(cursor);
        } else {
            // A slow answer is not a dead app: a busy host (or a stalled UI) can miss one
            // request. Only declare it gone when /health stays silent too.
            let stalled = std::time::Instant::now();
            if backend.recovers_within(Duration::from_secs(20)).await {
                eprintln!(
                    "victauri logs: app was unresponsive for {:.1}s — resumed",
                    stalled.elapsed().as_secs_f64()
                );
                continue;
            }
            println!("victauri logs: app went away (crashed, closed or rebuilding)");
            let went_away_ms = now_ms();
            let path = capture_path.clone().or_else(crate::run::latest_capture);
            if path.is_some() {
                // Show what the app printed in its final moments, then how it exited.
                return follow_capture(
                    &opts,
                    path,
                    Some(went_away_ms.saturating_sub(opts.interval_ms + 2000)),
                    Some(Duration::from_secs(30)),
                )
                .await;
            }
            return Ok(());
        }
    }
}

fn print_page(page: &Value, opts: &LogsOptions) {
    if let Some(entries) = page["entries"].as_array() {
        for e in entries {
            emit(&render_entry(e), opts.json, e);
        }
    }
}

fn print_digest(d: &Value) {
    println!(
        "buffered: {}   evicted: {}   counts: {}",
        d["buffered"], d["evicted"], d["buffered_counts"]
    );
    if let Some(p) = d["panics"].as_array().filter(|p| !p.is_empty()) {
        println!("\nPANICS ({}):", p.len());
        for e in p {
            println!("  {}", render_entry(e));
        }
    }
    if let Some(r) = d["recent_problems"].as_array().filter(|r| !r.is_empty()) {
        println!("\nRECENT WARNINGS/ERRORS:");
        for e in r {
            println!("  {}", render_entry(e));
        }
    }
    if let Some(r) = d["repeated"].as_array().filter(|r| !r.is_empty()) {
        println!("\nREPEATED:");
        for t in r {
            println!(
                "  {:>6}x {:<5} {}: {}",
                t["count"],
                t["level"].as_str().unwrap_or("").to_ascii_uppercase(),
                t["target"].as_str().unwrap_or(""),
                t["template"].as_str().unwrap_or("")
            );
        }
    }
    if let Some(t) = d["top_targets"].as_array().filter(|t| !t.is_empty()) {
        println!("\nNOISIEST TARGETS:");
        for x in t {
            println!(
                "  {:>6}  {}",
                x["count"],
                x["target"].as_str().unwrap_or("")
            );
        }
    }
    if let Some(h) = d["hint"].as_str() {
        println!("\n{h}");
    }
}

fn console_line_passes(l: &Value, opts: &LogsOptions) -> bool {
    if let Some(min) = &opts.level {
        // Lines whose level cannot be inferred are kept only when no level filter is set,
        // except the launcher's exit record, which always matters.
        let inferred = l["level"].as_str();
        if l["s"] != "exit" && !inferred.is_some_and(|lv| level_rank(lv) >= level_rank(min)) {
            return false;
        }
    }
    if let Some(g) = &opts.grep {
        return l["l"]
            .as_str()
            .is_some_and(|t| t.to_lowercase().contains(&g.to_lowercase()));
    }
    true
}

async fn follow_stdout_via_app(backend: &Backend, opts: &LogsOptions) -> Result<()> {
    let mut args =
        json!({"action": "stdout", "limit": opts.limit, "level": opts.level, "filter": opts.grep});
    let mut first = true;
    loop {
        let page = backend.logs(args.clone()).await?;
        if let Some(lines) = page["lines"].as_array() {
            for l in lines {
                emit(&render_console(l), opts.json, l);
            }
        }
        if !opts.follow {
            return Ok(());
        }
        args["since_seq"] = page["next_seq"].clone();
        if first {
            args["limit"] = 2000.into();
            first = false;
        }
        tokio::time::sleep(Duration::from_millis(opts.interval_ms)).await;
    }
}

/// Read (and with `--follow`, tail) a console capture file directly from disk.
/// `since_ts_ms` limits the initial print to records at/after that moment; with
/// `stop_after_idle` the follow ends at the app's exit record (or after that much silence).
async fn follow_capture(
    opts: &LogsOptions,
    path: Option<PathBuf>,
    since_ts_ms: Option<u64>,
    stop_after_idle: Option<Duration>,
) -> Result<()> {
    let path: PathBuf = match path {
        Some(p) => p,
        None => crate::run::latest_capture().context(
            "no `victauri run` console capture found — launch the app as `victauri run -- <dev command>`",
        )?,
    };
    eprintln!("victauri logs: console capture {}", path.display());
    let (lines, mut cursor) = read_capture(&path, None)?;
    let recent: Vec<&Value> = if let Some(ts) = since_ts_ms {
        lines
            .iter()
            .filter(|l| l["t"].as_u64().unwrap_or(0) >= ts)
            .collect()
    } else {
        let start = lines.len().saturating_sub(opts.limit);
        lines[start..].iter().collect()
    };
    let mut saw_exit = false;
    for l in recent {
        if console_line_passes(l, opts) {
            emit(&render_console(l), opts.json, l);
        }
        saw_exit |= l["s"] == "exit";
    }
    if !opts.follow || (saw_exit && stop_after_idle.is_some()) {
        return Ok(());
    }
    let mut idle = Duration::ZERO;
    loop {
        tokio::time::sleep(Duration::from_millis(opts.interval_ms)).await;
        let (lines, next) = read_capture(&path, Some(cursor))?;
        if lines.is_empty() {
            idle += Duration::from_millis(opts.interval_ms);
            if stop_after_idle.is_some_and(|max| idle >= max) {
                return Ok(());
            }
        } else {
            idle = Duration::ZERO;
        }
        for l in &lines {
            if console_line_passes(l, opts) {
                emit(&render_console(l), opts.json, l);
            }
            if l["s"] == "exit" && stop_after_idle.is_some() {
                return Ok(());
            }
        }
        cursor = next;
    }
}

/// Read JSONL capture records from `from` (or the whole file). Tolerates a
/// partially written last line and a file rotated under us.
fn read_capture(path: &Path, from: Option<u64>) -> Result<(Vec<Value>, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f =
        std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let len = f.metadata()?.len();
    let mut start = from.unwrap_or(0);
    if start > len {
        start = 0;
    }
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.take(32 * 1024 * 1024).read_to_end(&mut buf)?;
    let mut out = Vec::new();
    let mut consumed = 0usize;
    for chunk in buf.split_inclusive(|b| *b == b'\n') {
        if !chunk.ends_with(b"\n") {
            break;
        }
        consumed += chunk.len();
        if let Ok(v) = serde_json::from_slice::<Value>(chunk) {
            let mut v = v;
            if v.get("level").is_none()
                && let Some(text) = v["l"].as_str()
                && let Some(level) = infer_level(text)
            {
                v["level"] = level.into();
            }
            out.push(v);
        }
    }
    Ok((out, start + consumed as u64))
}

/// Same heuristic as the plugin's (`backend_logs::infer_level`), kept local so the CLI
/// does not depend on the plugin crate.
fn infer_level(text: &str) -> Option<&'static str> {
    if text.contains("panicked at") || text.starts_with("error:") || text.starts_with("error[") {
        return Some("error");
    }
    if text.starts_with("warning:") {
        return Some("warn");
    }
    for token in text
        .split(|c: char| c.is_whitespace() || c == '[' || c == ']')
        .filter(|t| !t.is_empty())
        .take(4)
    {
        match token {
            "TRACE" => return Some("trace"),
            "DEBUG" => return Some("debug"),
            "INFO" => return Some("info"),
            "WARN" | "WARNING" => return Some("warn"),
            "ERROR" | "FATAL" => return Some("error"),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_entry_is_one_readable_line() {
        let e = json!({
            "ts_ms": 1_758_969_130_161_u64,
            "level": "warn",
            "target": "4da::scoring",
            "message": "Scoring context build exceeded the 30s soft ceiling",
            "fields": {"caller": "backfill_unscored", "elapsed_ms": 41191},
            "spans": ["analysis{run_type=\"background_deep\"}"],
            "source": "tracing"
        });
        let line = render_entry(&e);
        assert!(line.contains("WARN  4da::scoring: Scoring context build exceeded"));
        assert!(line.contains("caller=\"backfill_unscored\""));
        assert!(line.contains("elapsed_ms=41191"));
        assert!(line.contains("[analysis{run_type=\"background_deep\"}]"));
        assert!(!line.contains('\n'));
        let multi = json!({"ts_ms": 0, "level": "warn", "target": "t", "message": "a\nb\r\nc"});
        assert!(
            !render_entry(&multi).contains('\n'),
            "multi-line messages stay on one line"
        );
    }

    #[test]
    fn capture_reader_infers_levels_and_skips_partial_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.jsonl");
        std::fs::write(
            &p,
            "{\"t\":1,\"s\":\"err\",\"l\":\"2026-09-27T10:00:00Z  WARN app: slow\"}\n{\"t\":2,\"s\":\"out\"",
        )
        .unwrap();
        let (lines, next) = read_capture(&p, None).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["level"], "warn");
        assert!(next > 0);
    }

    #[test]
    fn console_filter_keeps_exit_records_under_a_level_filter() {
        let opts = LogsOptions {
            app: None,
            level: Some("error".into()),
            target: None,
            grep: None,
            limit: 10,
            follow: false,
            digest: false,
            stdout: true,
            json: false,
            capture: None,
            interval_ms: 250,
        };
        assert!(console_line_passes(
            &json!({"s":"exit","l":"crashed: exit code 0xC0000409"}),
            &opts
        ));
        assert!(!console_line_passes(
            &json!({"s":"out","l":"hello","level":"info"}),
            &opts
        ));
        assert!(console_line_passes(
            &json!({"s":"err","l":"x","level":"error"}),
            &opts
        ));
    }
}
