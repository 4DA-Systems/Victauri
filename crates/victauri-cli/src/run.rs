//! `victauri run -- <command>` — launch an app (or its dev command) with its
//! stdout/stderr captured **out of process**.
//!
//! The launcher passes every byte through to this terminal unchanged, and also
//! appends each line to a JSONL capture file whose path it hands the app in
//! `VICTAURI_CONSOLE_LOG`. The embedded plugin serves that file as
//! `logs {action:"stdout"}`, and `victauri logs --stdout` reads it — even after
//! the app died, which is the point: an in-process capture dies with the
//! process, so a crash's last words (an abort message, a glibc heap-corruption
//! report, a Rust panic in `panic = "abort"` builds) would be lost. Here they are
//! already in the pipe, owned by this process, when the app goes away. The
//! final record says how the app exited (code, signal or NTSTATUS, decoded).
//!
//! Works for any app and any logger (`println!`, `tracing`, `log`,
//! `tauri-plugin-log`'s Stdout target, C libraries) with zero code changes, and
//! wraps whole dev commands (`npm run tauri dev`), so `cargo` build errors are
//! captured too.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};

/// Environment variable carrying the capture-file path to the app (read by the plugin).
pub const CONSOLE_CAPTURE_ENV: &str = "VICTAURI_CONSOLE_LOG";
/// Rotate the capture file past this size (the previous file is kept as `.1`).
const MAX_CAPTURE_BYTES: u64 = 64 * 1024 * 1024;
/// Longest line stored in the capture (the terminal still gets all of it).
const MAX_LINE_BYTES: usize = 64 * 1024;
/// stderr lines carried on the final `exit` record, so a crash's last words stay
/// attached to the crash even under a level filter.
const EXIT_TAIL_LINES: usize = 12;

/// Directory holding console captures: `<temp>/victauri/console`.
#[must_use]
pub fn capture_dir() -> PathBuf {
    std::env::temp_dir().join("victauri").join("console")
}

/// The most recently modified capture file, if any.
#[must_use]
pub fn latest_capture() -> Option<PathBuf> {
    std::fs::read_dir(capture_dir())
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

/// Remove ANSI escape sequences (CSI `ESC[...X`, OSC `ESC]...BEL`) and a trailing `\r`.
#[must_use]
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                // Parameters/intermediates until a final byte in 0x40..=0x7E.
                for n in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&n) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                while let Some(n) = chars.next() {
                    if n == '\u{7}' {
                        break;
                    }
                    if n == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {
                chars.next();
            }
        }
    }
    if out.ends_with('\r') {
        out.pop();
    }
    out
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Append-only JSONL sink shared by the stdout and stderr pumps.
struct Capture {
    path: PathBuf,
    file: std::fs::File,
    written: u64,
    recent_err: std::collections::VecDeque<String>,
}

impl Capture {
    fn create(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let file = open_private(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            written: 0,
            recent_err: std::collections::VecDeque::with_capacity(EXIT_TAIL_LINES),
        })
    }

    fn record(&mut self, stream: &str, line: &str) {
        let mut text = strip_ansi(line);
        if text.len() > MAX_LINE_BYTES {
            let mut cut = MAX_LINE_BYTES;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push_str("…[truncated]");
        }
        if stream == "err" && !text.trim().is_empty() {
            if self.recent_err.len() == EXIT_TAIL_LINES {
                self.recent_err.pop_front();
            }
            self.recent_err.push_back(text.clone());
        }
        let rec = serde_json::json!({ "t": now_ms(), "s": stream, "l": text });
        self.write_record(&rec);
    }

    /// The final record: how the app ended, plus its last stderr lines.
    fn record_exit(&mut self, summary: &str) {
        let rec = serde_json::json!({
            "t": now_ms(),
            "s": "exit",
            "l": summary,
            "tail": self.recent_err.iter().collect::<Vec<_>>(),
        });
        self.write_record(&rec);
    }

    fn write_record(&mut self, rec: &serde_json::Value) {
        let mut bytes = rec.to_string().into_bytes();
        bytes.push(b'\n');
        if self.written + bytes.len() as u64 > MAX_CAPTURE_BYTES {
            self.rotate();
        }
        // Capture failures must never disturb the app: ignore write errors.
        if self.file.write_all(&bytes).is_ok() {
            self.written += bytes.len() as u64;
        }
    }

    fn rotate(&mut self) {
        let old = self.path.with_extension("jsonl.1");
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::rename(&self.path, &old);
        if let Ok(f) = open_private(&self.path) {
            self.file = f;
            self.written = 0;
        }
    }
}

/// Create the capture file readable only by the current user where the OS
/// supports it (it can contain whatever the app logs, secrets included).
fn open_private(path: &Path) -> Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
        .with_context(|| format!("cannot open capture file {}", path.display()))
}

/// Copy `src` to `dst` byte-for-byte while recording each complete line.
fn pump<R: Read, W: Write>(
    src: R,
    mut dst: W,
    stream: &'static str,
    capture: &Arc<Mutex<Capture>>,
) {
    let mut reader = BufReader::with_capacity(64 * 1024, src);
    let mut buf = Vec::with_capacity(4096);
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let _ = dst.write_all(&buf);
                let _ = dst.flush();
                let line = String::from_utf8_lossy(&buf);
                let line = line.trim_end_matches('\n');
                if let Ok(mut c) = capture.lock() {
                    c.record(stream, line);
                }
            }
        }
    }
}

/// Resolve a bare program name the way a shell would (PATH + PATHEXT on Windows),
/// so `victauri run -- npm run tauri dev` finds `npm.cmd`.
fn resolve_program(program: &str) -> PathBuf {
    let p = Path::new(program);
    if p.components().count() > 1 || p.is_absolute() {
        return p.to_path_buf();
    }
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(str::to_ascii_lowercase)
            .collect()
    } else {
        Vec::new()
    };
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(program);
            if cfg!(windows) {
                if p.extension().is_some() && candidate.is_file() {
                    return candidate;
                }
                for ext in &exts {
                    let with_ext = dir.join(format!("{program}{ext}"));
                    if with_ext.is_file() {
                        return with_ext;
                    }
                }
            } else if candidate.is_file() {
                return candidate;
            }
        }
    }
    p.to_path_buf()
}

/// Human description of how the process ended, decoding the crash codes that
/// matter for a Tauri app (Windows NTSTATUS, Unix signals).
#[must_use]
pub fn describe_exit(status: ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            let name = match sig {
                6 => {
                    "SIGABRT (abort — e.g. a Rust panic with panic=\"abort\", or glibc heap-corruption detection)"
                }
                9 => "SIGKILL (killed)",
                11 => "SIGSEGV (segmentation fault)",
                15 => "SIGTERM (terminated)",
                2 => "SIGINT (interrupted)",
                _ => "signal",
            };
            return format!("killed by signal {sig}: {name}");
        }
    }
    match status.code() {
        Some(0) => "exited normally (code 0)".to_string(),
        Some(code) => {
            #[allow(clippy::cast_sign_loss)]
            let u = code as u32;
            let known = match u {
                0xC000_0005 => Some("STATUS_ACCESS_VIOLATION (invalid memory access)"),
                0xC000_0409 => Some(
                    "STATUS_STACK_BUFFER_OVERRUN (fail-fast — a Rust abort / panic=\"abort\", or detected corruption)",
                ),
                0xC000_00FD => Some("STATUS_STACK_OVERFLOW"),
                0xC000_0374 => Some("STATUS_HEAP_CORRUPTION"),
                0xC000_013A => Some("STATUS_CONTROL_C_EXIT (Ctrl+C)"),
                0x4001_0004 => Some("DBG_TERMINATE_PROCESS (killed)"),
                _ => None,
            };
            match known {
                Some(name) => format!("crashed: exit code 0x{u:08X} {name}"),
                None if code == 101 => {
                    "exited with code 101 (a Rust panic on the main thread)".to_string()
                }
                None => format!("exited with code {code}"),
            }
        }
        None => "exited without a status code".to_string(),
    }
}

/// Run `argv` under capture, returning the child's exit code.
///
/// # Errors
/// Fails if `argv` is empty, the capture file cannot be created, or the program
/// cannot be started.
pub fn run(argv: &[String], capture_file: Option<PathBuf>) -> Result<i32> {
    let Some((program, args)) = argv.split_first() else {
        bail!(
            "usage: victauri run -- <command> [args...]   e.g. victauri run -- npm run tauri dev"
        );
    };
    let path = capture_file.unwrap_or_else(|| {
        capture_dir().join(format!("{}-{}.jsonl", std::process::id(), now_ms()))
    });
    let capture = Arc::new(Mutex::new(Capture::create(&path)?));
    eprintln!(
        "victauri run: capturing stdout/stderr -> {}\n\
         victauri run: agents read it with `logs {{action:\"stdout\"}}` (app up) or \
         `victauri logs --stdout` (any time, even after a crash)",
        path.display()
    );

    // Ctrl+C reaches the child directly (same console / process group); the
    // launcher must survive it so it can record how the child ended.
    let _ = ctrlc::set_handler(|| {});

    let resolved = resolve_program(program);
    let mut child = Command::new(&resolved)
        .args(args)
        .env(CONSOLE_CAPTURE_ENV, &path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("cannot start {}", resolved.display()))?;

    let out = child.stdout.take().context("child stdout")?;
    let err = child.stderr.take().context("child stderr")?;
    let c1 = Arc::clone(&capture);
    let c2 = Arc::clone(&capture);
    let t_out = std::thread::spawn(move || pump(out, std::io::stdout(), "out", &c1));
    let t_err = std::thread::spawn(move || pump(err, std::io::stderr(), "err", &c2));

    let status = child.wait().context("waiting for the app")?;
    // Drain whatever the child wrote before it died — this is where crash output lives.
    let _ = t_out.join();
    let _ = t_err.join();

    let summary = describe_exit(status);
    if let Ok(mut c) = capture.lock() {
        c.record_exit(&summary);
    }
    eprintln!("victauri run: app {summary}");
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_color_and_osc_sequences() {
        assert_eq!(
            strip_ansi("\u{1b}[2m2026\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m app: hi\r"),
            "2026  INFO app: hi"
        );
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}text"), "text");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn capture_records_are_jsonl_and_rotate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.jsonl");
        let mut c = Capture::create(&path).unwrap();
        c.record("out", "\u{1b}[31mhello\u{1b}[0m");
        c.record("err", "boom");
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["s"], "out");
        assert_eq!(lines[0]["l"], "hello");
        assert_eq!(lines[1]["s"], "err");

        c.written = MAX_CAPTURE_BYTES;
        c.record("out", "after rotate");
        assert!(path.with_extension("jsonl.1").exists());
        let fresh = std::fs::read_to_string(&path).unwrap();
        assert_eq!(fresh.lines().count(), 1);
    }

    #[test]
    fn describe_exit_decodes_crash_codes() {
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            let s = ExitStatus::from_raw(0xC000_0409);
            assert!(describe_exit(s).contains("STACK_BUFFER_OVERRUN"));
            let s = ExitStatus::from_raw(101);
            assert!(describe_exit(s).contains("panic"));
            let s = ExitStatus::from_raw(0);
            assert!(describe_exit(s).contains("normally"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            let s = ExitStatus::from_raw(6); // raw wait status: killed by signal 6
            assert!(describe_exit(s).contains("SIGABRT"));
        }
    }

    /// A shell one-liner that prints to both streams and exits 3.
    fn shell(script_win: &str, script_unix: &str) -> Vec<String> {
        if cfg!(windows) {
            vec!["cmd".into(), "/C".into(), script_win.into()]
        } else {
            vec!["sh".into(), "-c".into(), script_unix.into()]
        }
    }

    #[test]
    fn run_passes_output_through_captures_both_streams_and_the_exit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.jsonl");
        let argv = shell(
            "echo hello-out & echo hello-err 1>&2 & exit 3",
            "echo hello-out; echo hello-err 1>&2; exit 3",
        );
        let code = run(&argv, Some(path.clone())).unwrap();
        assert_eq!(code, 3, "the child's exit code is propagated");
        let recs: Vec<serde_json::Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let find = |s: &str, needle: &str| {
            recs.iter()
                .any(|r| r["s"] == s && r["l"].as_str().unwrap().contains(needle))
        };
        assert!(find("out", "hello-out"), "{recs:?}");
        assert!(find("err", "hello-err"), "{recs:?}");
        let last = recs.last().unwrap();
        assert_eq!(last["s"], "exit");
        assert!(last["l"].as_str().unwrap().contains("code 3"), "{last}");
        assert!(
            last["tail"]
                .as_array()
                .unwrap()
                .iter()
                .any(|l| l.as_str().unwrap().contains("hello-err")),
            "the exit record carries the last stderr lines: {last}"
        );
    }

    #[test]
    fn run_hands_the_capture_path_to_the_child() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("env.jsonl");
        let argv = shell(
            "echo capture=%VICTAURI_CONSOLE_LOG%",
            "echo capture=$VICTAURI_CONSOLE_LOG",
        );
        run(&argv, Some(path.clone())).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("env.jsonl"),
            "the app must learn where its output is captured: {text}"
        );
    }
}
