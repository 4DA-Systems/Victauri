//! End-to-end tests of the real `victauri run` launcher and of `victauri logs` reading what it
//! captured: passthrough timing, exit codes, what happens to the app when the launcher is killed,
//! capture retention and rotation. Everything runs under a private temp root, so no real capture
//! or discovery entry on the machine is seen or touched.

use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

struct Temp(tempfile::TempDir);

impl Temp {
    fn new() -> Self {
        Self(tempfile::tempdir().expect("create isolated temp dir"))
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.path().join(name)
    }

    fn victauri(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_victauri"));
        for var in ["TMP", "TEMP", "TMPDIR", "XDG_RUNTIME_DIR"] {
            cmd.env(var, self.0.path());
        }
        for var in ["VICTAURI_PORT", "VICTAURI_AUTH_TOKEN", "VICTAURI_APP"] {
            cmd.env_remove(var);
        }
        cmd.env("HOME", self.0.path()).env_remove("XDG_STATE_HOME");
        cmd.current_dir(self.0.path());
        cmd.args(args);
        cmd
    }

    /// A script the shell runs: `.cmd` on Windows, `sh` on Unix. Returns the argv that runs it.
    fn script(&self, name: &str, windows: &str, unix: &str) -> Vec<String> {
        if cfg!(windows) {
            let path = self.path(&format!("{name}.cmd"));
            std::fs::write(&path, format!("@echo off\r\n{windows}\r\n")).unwrap();
            vec![path.display().to_string()]
        } else {
            let path = self.path(&format!("{name}.sh"));
            std::fs::write(&path, unix).unwrap();
            vec!["sh".to_string(), path.display().to_string()]
        }
    }

    /// A PowerShell script (Windows) that records its PID in `pid_file` and sleeps.
    #[cfg(windows)]
    fn pid_sleeper_ps1(&self, pid_file: &Path) -> PathBuf {
        let path = self.path("sleeper.ps1");
        std::fs::write(
            &path,
            format!(
                "Set-Content -Encoding ascii -Path '{}' -Value $PID\r\nStart-Sleep 60\r\n",
                pid_file.display()
            ),
        )
        .unwrap();
        path
    }
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_pid(file: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(pid) = std::fs::read_to_string(file)
            .ok()
            .and_then(|s| s.trim().parse().ok())
        {
            return pid;
        }
        assert!(Instant::now() < deadline, "the child never wrote its pid");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn kill_pid(pid: u32) {
    let pid = pid.to_string();
    let _ = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/F", "/PID", &pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    } else {
        Command::new("kill").args(["-9", &pid]).status()
    };
}

/// A prompt has no newline. `npm.cmd` under Ctrl+C prints `Terminate batch job (Y/N)? ` and waits:
/// the launcher held it back until a newline that only came after the user answered blind.
#[test]
fn a_prompt_without_a_newline_reaches_the_reader_while_the_child_waits() {
    let t = Temp::new();
    let cap = t.path("c.jsonl");
    let argv = t.script(
        "prompt",
        "<nul set /p =Enter password: \r\nping -n 9 127.0.0.1 >nul",
        "printf 'Enter password: '\nsleep 8\n",
    );
    let mut args = vec!["run", "--capture", cap.to_str().unwrap(), "--"];
    args.extend(argv.iter().map(String::as_str));
    let mut child = KillOnDrop(
        t.victauri(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut out = child.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut seen = Vec::new();
        let mut buf = [0u8; 256];
        while let Ok(n) = out.read(&mut buf) {
            if n == 0 {
                break;
            }
            seen.extend_from_slice(&buf[..n]);
            if String::from_utf8_lossy(&seen).contains("Enter password") {
                let _ = tx.send(());
                break;
            }
        }
    });
    rx.recv_timeout(Duration::from_secs(5))
        .expect("the prompt must reach the reader while the child is still waiting");
}

/// Passthrough is by chunk now, but the capture still stores whole lines, and a partial last line
/// is not lost when the child exits without ever ending it.
#[test]
fn the_capture_keeps_whole_lines_and_a_partial_last_line() {
    let t = Temp::new();
    let cap = t.path("c.jsonl");
    let argv = t.script(
        "lines",
        "echo first\r\necho second\r\n<nul set /p =no-newline-at-end\r\nexit /b 0",
        "echo first\necho second\nprintf 'no-newline-at-end'\n",
    );
    let mut args = vec!["run", "--capture", cap.to_str().unwrap(), "--"];
    args.extend(argv.iter().map(String::as_str));
    let out = t.victauri(&args).stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no-newline-at-end"), "{stdout}");
    let recs: Vec<serde_json::Value> = std::fs::read_to_string(&cap)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let lines: Vec<&str> = recs
        .iter()
        .filter(|r| r["s"] == "out")
        .map(|r| r["l"].as_str().unwrap().trim_end())
        .collect();
    assert_eq!(lines, ["first", "second", "no-newline-at-end"], "{recs:?}");
    assert_eq!(recs.last().unwrap()["s"], "exit");
}

/// `victauri run -- … 2>&1 | head`: the launcher's final message panicked on the closed pipe, so
/// the child's exit code became 1 (the capture still said 7).
#[test]
fn a_closed_stderr_does_not_change_the_exit_code() {
    let t = Temp::new();
    let cap = t.path("c.jsonl");
    let argv = t.script(
        "exit7",
        "ping -n 3 127.0.0.1 >nul\r\nexit /b 7",
        "sleep 2\nexit 7\n",
    );
    let mut args = vec!["run", "--capture", cap.to_str().unwrap(), "--"];
    args.extend(argv.iter().map(String::as_str));
    let mut child = t
        .victauri(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut first = [0u8; 16];
    let _ = stderr.read(&mut first).unwrap();
    drop(stderr);
    let status = child.wait().unwrap();
    assert_eq!(
        status.code(),
        Some(7),
        "the child's code must be propagated"
    );
}

/// A child that ignores SIGTERM held `victauri run` forever: the signal was forwarded once and
/// nothing followed it. `signals` stop signals are sent `apart` apart; the launcher must then end
/// within `within`, having recorded the kill.
#[cfg(not(windows))]
fn stubborn_child_is_killed(signals: usize, apart: Duration, within: Duration) {
    let t = Temp::new();
    let cap = t.path("c.jsonl");
    let pid_file = t.path("pid");
    let argv = t.script(
        "app",
        "",
        &format!(
            "trap '' TERM INT\necho $$ > '{}'\nwhile :; do sleep 1; done\n",
            pid_file.display()
        ),
    );
    let mut args = vec!["run", "--capture", cap.to_str().unwrap(), "--"];
    args.extend(argv.iter().map(String::as_str));
    let mut launcher = KillOnDrop(
        t.victauri(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let app_pid = wait_for_pid(&pid_file);
    let start = Instant::now();
    for i in 0..signals {
        if i > 0 {
            std::thread::sleep(apart);
        }
        Command::new("kill")
            .args(["-TERM", &launcher.0.id().to_string()])
            .status()
            .unwrap();
    }
    let ended = loop {
        if launcher.0.try_wait().unwrap().is_some() {
            break true;
        }
        if start.elapsed() > within {
            break false;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if !ended {
        kill_pid(app_pid);
    }
    assert!(ended, "still running after {within:?}");
    let capture = std::fs::read_to_string(&cap).unwrap();
    assert!(capture.contains("SIGKILL"), "{capture}");
}

#[cfg(not(windows))]
#[test]
fn a_child_ignoring_sigterm_is_killed_in_the_end() {
    stubborn_child_is_killed(1, Duration::ZERO, Duration::from_secs(20));
}

#[cfg(not(windows))]
#[test]
fn a_second_stop_signal_kills_at_once() {
    stubborn_child_is_killed(2, Duration::from_millis(1_500), Duration::from_secs(6));
}

/// `timeout`, `npm` and `concurrently` signal the launcher AND the group it is in, so one stop
/// reaches the launcher twice within moments, with the child already shutting down on its copy.
/// The echo used to count as "kill it now", which cut every such graceful shutdown short.
#[cfg(not(windows))]
#[test]
fn a_stop_signal_echoed_by_a_wrapper_leaves_a_graceful_shutdown_alone() {
    let t = Temp::new();
    let cap = t.path("c.jsonl");
    let pid_file = t.path("pid");
    let marker = t.path("graceful");
    let argv = t.script(
        "app",
        "",
        &format!(
            "trap 'sleep 1; echo done > \"{}\"; exit 0' TERM\necho $$ > '{}'\nwhile :; do sleep 0.1; done\n",
            marker.display(),
            pid_file.display()
        ),
    );
    let mut args = vec!["run", "--capture", cap.to_str().unwrap(), "--"];
    args.extend(argv.iter().map(String::as_str));
    let mut launcher = KillOnDrop(
        t.victauri(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let app_pid = wait_for_pid(&pid_file);
    let kill = |pid: u32| {
        Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .unwrap();
    };
    kill(launcher.0.id());
    kill(app_pid);
    std::thread::sleep(Duration::from_millis(100));
    kill(launcher.0.id());
    let start = Instant::now();
    while launcher.0.try_wait().unwrap().is_none() && start.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(marker.exists(), "the child was killed during its shutdown");
    let capture = std::fs::read_to_string(&cap).unwrap();
    assert!(!capture.contains("SIGKILL"), "{capture}");
}

/// An agent harness or `timeout` that stops the launcher left the app holding its port and its
/// single-instance lock. On Windows the launcher is killed outright (no handler can run) and the
/// job object takes the whole tree; on Unix SIGTERM is forwarded and the exit is recorded.
#[test]
fn stopping_the_launcher_takes_the_app_with_it() {
    let t = Temp::new();
    let cap = t.path("c.jsonl");
    let pid_file = t.path("pid");
    #[cfg(windows)]
    let argv = {
        // cmd.exe -> powershell.exe: the shape of `npm.cmd` -> the app, so the GRANDCHILD counts.
        let ps1 = t.pid_sleeper_ps1(&pid_file);
        t.script(
            "app",
            &format!(
                "powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\"",
                ps1.display()
            ),
            "",
        )
    };
    #[cfg(not(windows))]
    let argv = t.script(
        "app",
        "",
        &format!("echo $$ > '{}'\nexec sleep 60\n", pid_file.display()),
    );
    let mut args = vec!["run", "--capture", cap.to_str().unwrap(), "--"];
    args.extend(argv.iter().map(String::as_str));
    let mut launcher = KillOnDrop(
        t.victauri(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let app_pid = wait_for_pid(&pid_file);
    assert!(victauri_test::process::is_own_live_process(app_pid));

    #[cfg(windows)]
    launcher.0.kill().unwrap();
    #[cfg(not(windows))]
    Command::new("kill")
        .args(["-TERM", &launcher.0.id().to_string()])
        .status()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut gone = false;
    while Instant::now() < deadline {
        if !victauri_test::process::is_own_live_process(app_pid) {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !gone {
        kill_pid(app_pid);
    }
    assert!(gone, "the app outlived the launcher");

    // A signal can be handled: the launcher waits for the app and records how it ended.
    #[cfg(not(windows))]
    {
        let status = launcher.0.wait().unwrap();
        let capture = std::fs::read_to_string(&cap).unwrap();
        assert!(capture.contains("\"s\":\"exit\""), "{capture} ({status:?})");
    }
}

/// `victauri run` under a default capture in the private directory, printing one line and then
/// sleeping, with its output read away. The capture's name holds the launcher's pid, which is what
/// tells a reader whose capture it is.
fn run_a_sleeping_app(t: &Temp) -> (KillOnDrop, PathBuf) {
    let argv = t.script(
        "app",
        "echo last words\r\nping -n 60 127.0.0.1 >nul",
        "echo last words\nexec sleep 60\n",
    );
    let mut args = vec!["run", "--"];
    args.extend(argv.iter().map(String::as_str));
    let launcher = KillOnDrop(
        t.victauri(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let name = format!("{}-", launcher.0.id());
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let capture = captures_under(t)
            .into_iter()
            .find(|p| p.file_name().unwrap().to_string_lossy().starts_with(&name));
        if let Some(capture) = capture
            && std::fs::read_to_string(&capture).is_ok_and(|text| text.contains("last words"))
        {
            return (launcher, capture);
        }
        assert!(Instant::now() < deadline, "no capture appeared");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The default captures under `t`'s private temp root, whichever discovery root took them.
fn captures_under(t: &Temp) -> Vec<PathBuf> {
    let roots = std::fs::read_dir(t.0.path())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("victauri")
        });
    roots
        .flat_map(|root| {
            std::fs::read_dir(root.join("console"))
                .into_iter()
                .flatten()
        })
        .flatten()
        .map(|e| e.path())
        .collect()
}

/// A killed `victauri run` writes no exit record, on Windows always and on Unix after SIGKILL. The
/// launcher cannot say so, but a reader can: the capture's pid is dead and nothing says how it ended.
#[test]
fn a_reader_says_when_the_launcher_was_killed() {
    let t = Temp::new();
    let (mut launcher, capture) = run_a_sleeping_app(&t);
    launcher.0.kill().unwrap();
    launcher.0.wait().unwrap();

    let out = t.victauri(&["logs", "--stdout"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{out:?}");
    assert!(text.contains("last words"), "{text}");
    assert!(text.contains("EXIT"), "{text}");
    assert!(text.contains("killed"), "{text}");
    assert!(text.contains("unknown"), "{text}");
    assert!(text.contains(&launcher.0.id().to_string()), "{text}");

    // In JSON too, and it is not written into the capture: it is what the reader worked out.
    let out = t
        .victauri(&["logs", "--stdout", "--json"])
        .output()
        .unwrap();
    let last = String::from_utf8_lossy(&out.stdout)
        .lines()
        .last()
        .unwrap()
        .to_string();
    let record: serde_json::Value = serde_json::from_str(&last).unwrap();
    assert_eq!(record["s"], "exit", "{record}");
    assert_eq!(record["unknown"], true, "{record}");
    assert!(
        !std::fs::read_to_string(&capture)
            .unwrap()
            .contains("unknown")
    );

    // A follower has nothing to wait for.
    let started = Instant::now();
    let out = t
        .victauri(&["logs", "--stdout", "--follow"])
        .output()
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("unknown"));
}

/// A follower attached while the launcher runs learns of its death, without an exit record to wait for.
#[test]
fn a_follower_says_when_the_launcher_is_killed_under_it() {
    let t = Temp::new();
    let (mut launcher, _capture) = run_a_sleeping_app(&t);
    let follower = t
        .victauri(&["logs", "--stdout", "--follow", "--interval-ms", "100"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(victauri_test::process::is_own_live_process(follower.id()));
    launcher.0.kill().unwrap();
    launcher.0.wait().unwrap();
    let pid = follower.id();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(follower.wait_with_output());
    });
    let Ok(out) = rx.recv_timeout(Duration::from_secs(20)) else {
        kill_pid(pid);
        panic!("the follower kept waiting for a launcher that is gone");
    };
    let text = String::from_utf8_lossy(&out.unwrap().stdout).into_owned();
    assert!(text.contains("last words"), "{text}");
    assert!(text.contains("unknown"), "{text}");
}

/// A launcher that ended normally recorded how the app ended: nothing is unknown, and a capture
/// whose launcher is alive is not read as a dead one.
#[test]
fn a_recorded_exit_and_a_live_launcher_are_not_called_unknown() {
    let t = Temp::new();
    let (mut launcher, _capture) = run_a_sleeping_app(&t);
    let out = t.victauri(&["logs", "--stdout"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("last words") && !text.contains("unknown"),
        "{text}"
    );
    launcher.0.kill().unwrap();
    launcher.0.wait().unwrap();

    let t = Temp::new();
    let argv = t.script("done", "echo bye\r\nexit /b 4", "echo bye\nexit 4\n");
    let mut args = vec!["run", "--"];
    args.extend(argv.iter().map(String::as_str));
    let status = t
        .victauri(&args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
        .status;
    assert_eq!(status.code(), Some(4));
    let out = t.victauri(&["logs", "--stdout"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("code 4") && !text.contains("unknown"),
        "{text}"
    );
}

/// SIGKILL cannot be handled, so nothing in this process can stop the app. A small helper holding
/// a pipe from the launcher can: the pipe closes when the launcher dies, however it dies.
#[cfg(not(windows))]
#[test]
fn a_launcher_killed_outright_takes_the_app_with_it() {
    let t = Temp::new();
    let cap = t.path("c.jsonl");
    let pid_file = t.path("pid");
    let argv = t.script(
        "app",
        "",
        &format!("echo $$ > '{}'\nexec sleep 60\n", pid_file.display()),
    );
    let mut args = vec!["run", "--capture", cap.to_str().unwrap(), "--"];
    args.extend(argv.iter().map(String::as_str));
    let mut launcher = KillOnDrop(
        t.victauri(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let app_pid = wait_for_pid(&pid_file);
    launcher.0.kill().unwrap();
    launcher.0.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while victauri_test::process::is_own_live_process(app_pid) {
        if Instant::now() > deadline {
            kill_pid(app_pid);
            panic!("the app outlived a launcher killed with SIGKILL");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// ... and one that ignores SIGTERM too, as `victauri run` itself ends such a child.
#[cfg(not(windows))]
#[test]
fn a_launcher_killed_outright_takes_a_stubborn_app_with_it() {
    let t = Temp::new();
    let cap = t.path("c.jsonl");
    let pid_file = t.path("pid");
    let argv = t.script(
        "app",
        "",
        &format!(
            "trap '' TERM\necho $$ > '{}'\nwhile :; do sleep 1; done\n",
            pid_file.display()
        ),
    );
    let mut args = vec!["run", "--capture", cap.to_str().unwrap(), "--"];
    args.extend(argv.iter().map(String::as_str));
    let mut launcher = KillOnDrop(
        t.victauri(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let app_pid = wait_for_pid(&pid_file);
    launcher.0.kill().unwrap();
    launcher.0.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while victauri_test::process::is_own_live_process(app_pid) {
        if Instant::now() > deadline {
            kill_pid(app_pid);
            panic!("a child ignoring SIGTERM outlived its killed launcher");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The helper is for a launcher that dies: when the app exits it goes too, or every run would leave
/// a shell behind, waiting to signal a pid that may be someone else's by then.
#[cfg(not(windows))]
#[test]
fn the_app_exiting_ends_the_helper() {
    fn helper_listed(app_pid: u32) -> bool {
        let ps = Command::new("ps")
            .args(["-ww", "-eo", "args"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&ps.stdout)
            .lines()
            .any(|l| l.contains("victauri-run-watch") && l.contains(&format!(" {app_pid} ")))
    }
    let t = Temp::new();
    let cap = t.path("c.jsonl");
    let pid_file = t.path("pid");
    let argv = t.script(
        "app",
        "",
        &format!("echo $$ > '{}'\nsleep 3\nexit 0\n", pid_file.display()),
    );
    let mut args = vec!["run", "--capture", cap.to_str().unwrap(), "--"];
    args.extend(argv.iter().map(String::as_str));
    let mut launcher = KillOnDrop(
        t.victauri(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let app_pid = wait_for_pid(&pid_file);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !helper_listed(app_pid) {
        assert!(Instant::now() < deadline, "the helper is not running");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(launcher.0.wait().unwrap().success());
    assert!(!helper_listed(app_pid), "the helper outlived the app");
}

/// The job object is only for a launcher that DIES: something the app deliberately left running
/// when it exited normally (a dev server, a daemon) is not ours to kill.
#[test]
fn a_process_the_app_left_behind_survives_a_normal_exit() {
    let t = Temp::new();
    let cap = t.path("c.jsonl");
    let pid_file = t.path("pid");
    #[cfg(windows)]
    let argv = {
        let ps1 = t.pid_sleeper_ps1(&pid_file);
        t.script(
            "app",
            &format!(
                "start \"\" /b powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\"\r\n:wait\r\nif not exist \"{}\" (ping -n 2 127.0.0.1 >nul & goto wait)\r\nexit /b 0",
                ps1.display(),
                pid_file.display()
            ),
            "",
        )
    };
    #[cfg(not(windows))]
    let argv = t.script(
        "app",
        "",
        &format!(
            "sleep 60 >/dev/null 2>&1 &\necho $! > '{}'\nexit 0\n",
            pid_file.display()
        ),
    );
    let mut args = vec!["run", "--capture", cap.to_str().unwrap(), "--"];
    args.extend(argv.iter().map(String::as_str));
    // Not `.output()`: the helper inherits the pipes and would hold them open for a minute.
    let status = t
        .victauri(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "{status:?}");
    let helper = wait_for_pid(&pid_file);
    std::thread::sleep(Duration::from_millis(500));
    let alive = victauri_test::process::is_own_live_process(helper);
    kill_pid(helper);
    assert!(
        alive,
        "a normal exit must not kill what the app left running"
    );
}

/// Each run left a capture forever: up to 128 MB, holding whatever the app printed.
#[test]
fn old_captures_are_pruned_and_the_newest_survive() {
    let t = Temp::new();
    let run_once = || {
        let argv = t.script("noop", "exit /b 0", "exit 0\n");
        let mut args = vec!["run", "--"];
        args.extend(argv.iter().map(String::as_str));
        let out = t.victauri(&args).stdin(Stdio::null()).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        out
    };
    // The first run creates the private directory (owner-checked) the seeds go into, and says
    // where (the discovery root differs by platform).
    let first = run_once();
    let stderr = String::from_utf8_lossy(&first.stderr);
    let announced = stderr
        .lines()
        .find_map(|l| l.split_once(" -> ").map(|(_, path)| PathBuf::from(path)))
        .expect("the launcher names the capture");
    let dir = announced.parent().unwrap().to_path_buf();
    assert!(
        dir.starts_with(t.0.path())
            || dir
                .canonicalize()
                .unwrap()
                .starts_with(t.0.path().canonicalize().unwrap()),
        "the capture directory is under the temp root: {}",
        dir.display()
    );

    let now = SystemTime::now();
    let age = |days: u64| now - Duration::from_secs(days * 24 * 3600);
    let seed = |name: &str, modified: SystemTime| {
        let path = dir.join(name);
        std::fs::write(&path, "{}\n").unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(modified).unwrap();
        path
    };
    // PIDs far above anything live, so these all read as dead.
    let old = seed("2000000001-1.jsonl", age(30));
    let old_rotated = seed("2000000001-1.jsonl.1", age(30));
    let recent = seed("2000000002-2.jsonl", age(1));
    // The live process's capture is never deleted, however old it looks.
    let live = seed(&format!("{}-3.jsonl", std::process::id()), age(30));
    // Not a capture name: not ours to delete.
    let foreign = seed("notes.jsonl", age(30));
    // Past the count bound: only the newest survive even when they are all recent.
    let extra: Vec<PathBuf> = (0..40)
        .map(|i| {
            seed(
                &format!("{}-{i}.jsonl", 2_000_000_100 + i),
                age(2) + Duration::from_secs(i),
            )
        })
        .collect();

    run_once();

    assert!(!old.exists(), "a month-old capture is pruned");
    assert!(!old_rotated.exists(), "with its rotated file");
    assert!(recent.exists(), "a recent capture stays");
    assert!(live.exists(), "the capture of a live pid stays");
    assert!(foreign.exists(), "a file that is not a capture stays");
    assert!(!extra[0].exists(), "past the count bound, the oldest goes");
    assert!(extra[39].exists(), "the newest stays");
}

fn capture_line(seq: u32) -> String {
    format!(
        "{}\n",
        serde_json::json!({"t": 1, "s": "out", "l": format!("seq {seq} {}", "x".repeat(80))})
    )
}

/// Follow a capture through a rotation done exactly as the launcher does it (rename to
/// `.jsonl.1`, start a new file): every line must arrive. `new_file_bigger` covers the rotation
/// that a length check cannot see: the new file has already grown past the follower's cursor.
fn follow_across_rotation(new_file_bigger: bool) {
    let t = Temp::new();
    let cap = t.path("rot.jsonl");
    std::fs::write(&cap, capture_line(0)).unwrap();
    let mut child = KillOnDrop(
        t.victauri(&[
            "logs",
            "--capture",
            cap.to_str().unwrap(),
            "--follow",
            "--json",
            "-n",
            "0",
            "--interval-ms",
            "1000",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap(),
    );
    // Its first read is done just after it names the capture; the next poll is a second away.
    let mut err = BufReader::new(child.0.stderr.take().unwrap());
    let mut line = String::new();
    err.read_line(&mut line).unwrap();
    assert!(line.contains("console capture"), "{line}");
    std::thread::sleep(Duration::from_millis(300));

    // Unread tail of the old file, then the rotation, then the new file.
    let mut tail = String::new();
    for seq in 1..=20 {
        tail.push_str(&capture_line(seq));
    }
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new().append(true).open(&cap).unwrap();
        f.write_all(tail.as_bytes()).unwrap();
    }
    std::fs::rename(&cap, cap.with_extension("jsonl.1")).unwrap();
    let last: u32 = if new_file_bigger { 100 } else { 22 };
    let mut fresh = String::new();
    for seq in 21..=last {
        fresh.push_str(&capture_line(seq));
    }
    std::fs::write(&cap, fresh).unwrap();

    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for l in BufReader::new(stdout).lines() {
            let Ok(l) = l else { break };
            if tx.send(l).is_err() {
                break;
            }
        }
    });
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && seen.len() < last as usize {
        if let Ok(l) = rx.recv_timeout(Duration::from_millis(200)) {
            let v: serde_json::Value = serde_json::from_str(&l).unwrap();
            let text = v["l"].as_str().unwrap().to_string();
            seen.push(text.split(' ').nth(1).unwrap().parse::<u32>().unwrap());
        }
    }
    let want: Vec<u32> = (1..=last).collect();
    assert_eq!(seen, want, "every line, in order, across the rotation");
}

#[test]
fn a_follower_loses_nothing_when_the_new_file_is_smaller_than_its_cursor() {
    follow_across_rotation(false);
}

#[test]
fn a_follower_loses_nothing_when_the_new_file_is_already_past_its_cursor() {
    follow_across_rotation(true);
}

/// A rotation the follower cannot read through (the rotated file was replaced too, by a second
/// rotation between two polls) must be SAID, not silently skipped.
#[test]
fn a_follower_says_so_when_the_rotated_file_is_gone() {
    let t = Temp::new();
    let cap = t.path("rot.jsonl");
    std::fs::write(&cap, capture_line(0)).unwrap();
    let mut child = KillOnDrop(
        t.victauri(&[
            "logs",
            "--capture",
            cap.to_str().unwrap(),
            "--follow",
            "--json",
            "-n",
            "0",
            "--interval-ms",
            "1000",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap(),
    );
    let mut err = BufReader::new(child.0.stderr.take().unwrap());
    let mut line = String::new();
    err.read_line(&mut line).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    std::fs::write(&cap, capture_line(1)).unwrap();
    std::fs::write(cap.with_extension("jsonl.1"), capture_line(2)).unwrap();

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for l in err.lines() {
            let Ok(l) = l else { break };
            if tx.send(l).is_err() {
                break;
            }
        }
    });
    let said = rx
        .recv_timeout(Duration::from_secs(8))
        .expect("the follower must report the lost lines");
    assert!(said.contains("rotated"), "{said}");
}

/// A capture does not record whose it is, so with a selector (`--app`, `VICTAURI_APP`) or a pin
/// (`VICTAURI_PORT`) the newest one could be another app's. `--app` was refused; the env selector
/// printed the other app's output.
#[test]
fn a_selector_in_the_environment_stops_logs_showing_another_apps_capture() {
    let t = Temp::new();
    let argv = t.script(
        "other",
        "echo output-of-another-app",
        "echo output-of-another-app\n",
    );
    let mut args = vec!["run", "--"];
    args.extend(argv.iter().map(String::as_str));
    assert!(
        t.victauri(&args)
            .stdin(Stdio::null())
            .output()
            .unwrap()
            .status
            .success()
    );

    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    for (var, value) in [
        ("VICTAURI_APP", "com.mine.app".to_string()),
        ("VICTAURI_PORT", closed.to_string()),
    ] {
        let mut cmd = t.victauri(&["logs"]);
        cmd.env(var, &value).stdin(Stdio::null());
        let out = cmd.output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(!out.status.success(), "{var}: {out:?}");
        assert!(
            !stdout.contains("output-of-another-app"),
            "{var}: showed another app's output: {stdout}"
        );
    }
    // No selector: the documented fallback still works.
    let out = t.victauri(&["logs"]).stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("output-of-another-app"));
}

fn symlink_file(target: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

/// A cloned repo's `app.jsonl -> ~/.ssh/authorized_keys`: `--capture` appended the app's output
/// to the victim. `record`, `init` and `setup` refuse this.
#[test]
fn capture_through_a_symlink_is_refused() {
    let t = Temp::new();
    let outside = Temp::new();
    let victim = outside.path("victim");
    std::fs::write(&victim, "PRECIOUS").unwrap();
    let link = t.path("app.jsonl");
    if !symlink_file(&victim, &link) {
        eprintln!("skipped: cannot create a symlink here");
        return;
    }
    let argv = t.script("hi", "echo hi", "echo hi\n");
    let mut args = vec!["run", "--capture", "app.jsonl", "--"];
    args.extend(argv.iter().map(String::as_str));
    let out = t.victauri(&args).stdin(Stdio::null()).output().unwrap();
    assert!(!out.status.success(), "{out:?}");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "PRECIOUS");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("symbolic link"),
        "{out:?}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("hi"),
        "the command must not have run"
    );
}
