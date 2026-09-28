//! Process liveness for discovery: is `pid` a live process owned by the current user?
//! (A copy of `victauri-test`'s `process` module; the watchdog does not depend on it.)
//!
//! Discovery trusts a `<temp>/victauri/<pid>/` entry only while its PID is alive, so this
//! check decides where a Bearer token is sent. It must be exact (PID 12 is not PID 123),
//! scoped to our own user (another user's process that inherited a stale PID must not make
//! an entry look live), and cheap — the CLI bridge polls it every 1.5 s. The previous
//! Windows check spawned `tasklist` and substring-matched its output: it failed all three
//! (measured 0.5 s per call on an idle machine, up to 40 s filtered by user under load).
//!
//! It must also never report a live process as DEAD merely because the check itself could not
//! run: discovery readers delete entries whose owner is dead, and the watchdog fires its
//! recovery command, so a false "dead" destroys a running app's only discovery entry (round-4
//! audit R4-DISC1: `/bin/kill` does not exist on NixOS/Guix/minimal containers, which made
//! every live PID read dead). [`liveness`] therefore distinguishes "definitely gone" from
//! "could not tell".
//!
//! Same-user PID reuse remains a documented residual: discovery liveness is PID-based.

/// What is known about the process that owns a discovery entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Liveness {
    /// A live process owned by the current user — its discovery token may be used.
    Own,
    /// A live process owned by a DIFFERENT account (the PID was recycled). Never send it a
    /// token; the entry is not ours to use.
    OtherUser,
    /// The process exists, but its owner could not be verified — e.g. an elevated app seen
    /// from a non-elevated client on Windows. Not trusted for token use, never deleted.
    Unverified,
    /// The process is definitely not running.
    Dead,
    /// No liveness mechanism worked, so nothing is known. Treat as "possibly alive": never
    /// delete, never trust for a token.
    Unknown,
}

impl Liveness {
    /// Only a definitely-dead owner makes a discovery entry safe to delete.
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn is_dead(self) -> bool {
        self == Self::Dead
    }
}

/// Liveness of `pid` relative to the current user (see [`Liveness`]).
#[must_use]
pub fn liveness(pid: u32) -> Liveness {
    imp::liveness(pid)
}

/// Whether `pid` is a live process owned by the current user.
#[cfg_attr(not(test), allow(dead_code))]
#[must_use]
pub fn is_own_live_process(pid: u32) -> bool {
    liveness(pid) == Liveness::Own
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use std::ffi::c_void;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, GetLastError, HANDLE,
        STILL_ACTIVE,
    };
    use windows_sys::Win32::Security::{EqualSid, GetTokenInformation, TOKEN_QUERY, TokenUser};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    use super::Liveness;

    /// Closes a kernel handle on drop.
    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a handle this module opened and has not closed.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// The `TOKEN_USER` of a process, as the raw (8-aligned) buffer it lives in.
    fn token_user(process: HANDLE) -> Option<Vec<u64>> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: `process` is a valid process handle; `token` is a valid out-pointer.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return None;
        }
        let token = Handle(token);
        let mut len = 0u32;
        // SAFETY: a null buffer of length 0 is the documented size probe; `len` is valid.
        unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut len) };
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: `buf` is at least `len` bytes and 8-aligned (TOKEN_USER holds pointers).
        let ok = unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buf.as_mut_ptr().cast::<c_void>(),
                len,
                &mut len,
            )
        };
        (ok != 0).then_some(buf)
    }

    /// The SID pointer inside a `TOKEN_USER` buffer from [`token_user`].
    fn sid(buf: &[u64]) -> *mut c_void {
        // SAFETY: `buf` holds a TOKEN_USER written by GetTokenInformation; its first field
        // is SID_AND_ATTRIBUTES, whose first field is the SID pointer (into `buf` itself).
        unsafe { *buf.as_ptr().cast::<*mut c_void>() }
    }

    pub(super) fn liveness(pid: u32) -> Liveness {
        if pid == 0 {
            return Liveness::Dead;
        }
        // SAFETY: plain FFI call; a null return is classified below.
        let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if raw.is_null() {
            // SAFETY: reads the calling thread's last-error value; no preconditions.
            return match unsafe { GetLastError() } {
                // No process with that id.
                ERROR_INVALID_PARAMETER => Liveness::Dead,
                // It exists, but we may not even open it (a protected / higher-integrity
                // process): alive, owner unverifiable.
                ERROR_ACCESS_DENIED => Liveness::Unverified,
                _ => Liveness::Unknown,
            };
        }
        let process = Handle(raw);
        let mut code = 0u32;
        // SAFETY: `process.0` is a valid handle; `code` is a valid out-pointer.
        if unsafe { GetExitCodeProcess(process.0, &mut code) } == 0 {
            return Liveness::Unknown;
        }
        if code != STILL_ACTIVE as u32 {
            return Liveness::Dead;
        }
        // SAFETY: the pseudo-handle of the current process needs no closing.
        let me = unsafe { GetCurrentProcess() };
        let Some(ours) = token_user(me) else {
            return Liveness::Unknown;
        };
        // A live process whose token we cannot read (plausibly an elevated app seen from a
        // non-elevated client) EXISTS: it must never be reported dead (that deletes its
        // discovery entry and fires watchdog recovery), but it is not verified as ours.
        let Some(theirs) = token_user(process.0) else {
            return Liveness::Unverified;
        };
        // SAFETY: both SIDs point into live TOKEN_USER buffers held above.
        if unsafe { EqualSid(sid(&theirs), sid(&ours)) } != 0 {
            Liveness::Own
        } else {
            Liveness::OtherUser
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::path::Path;
    use std::process::{Command, Stdio};

    use super::Liveness;

    /// Where `kill` lives, most common first. NixOS and Guix have no `/bin/kill` or
    /// `/usr/bin/kill`; their system profiles are listed explicitly rather than trusting
    /// `PATH` (a hijacked `PATH` must not decide where a token goes).
    const KILL_CANDIDATES: &[&str] = &[
        "/bin/kill",
        "/usr/bin/kill",
        "/run/current-system/sw/bin/kill",
        "/run/current-system/profile/bin/kill",
    ];
    /// Shells whose builtin `kill` is the fallback when no `kill` binary exists (most minimal
    /// images still ship `/bin/sh`; NixOS and Guix always provide it).
    const SH_CANDIDATES: &[&str] = &[
        "/bin/sh",
        "/usr/bin/sh",
        "/run/current-system/sw/bin/sh",
        "/run/current-system/profile/bin/sh",
    ];

    pub(super) fn liveness(pid: u32) -> Liveness {
        liveness_with(pid, KILL_CANDIDATES, SH_CANDIDATES, Path::new("/proc"))
    }

    /// Testable core: try a `kill` binary, then a shell's builtin `kill`, then `/proc`.
    pub(super) fn liveness_with(
        pid: u32,
        kill_candidates: &[&str],
        sh_candidates: &[&str],
        proc_root: &Path,
    ) -> Liveness {
        // `kill -0 0` signals our process group and a pid above i32::MAX is read as negative
        // (`-1` = every process we may signal): neither is a real discovery PID.
        if pid == 0 || i32::try_from(pid).is_err() {
            return Liveness::Dead;
        }
        let pid_s = pid.to_string();

        let signalled = kill_candidates
            .iter()
            .filter(|p| Path::new(p).is_file())
            .find_map(|kill| run_quietly(Command::new(kill).args(["-0", &pid_s])))
            .or_else(|| {
                sh_candidates
                    .iter()
                    .filter(|p| Path::new(p).is_file())
                    .find_map(|sh| {
                        // `$1` keeps the pid out of the script text; `kill` is the builtin.
                        run_quietly(Command::new(sh).args([
                            "-c",
                            "kill -0 \"$1\"",
                            "victauri-liveness",
                            &pid_s,
                        ]))
                    })
            });

        match signalled {
            // `kill -0` succeeds only for a live process we may signal: our own (unless we
            // are root — a documented residual, as before).
            Some(true) => Liveness::Own,
            // It ran and failed: no such process (ESRCH), or not ours to signal (EPERM, a
            // recycled PID). /proc, when present, tells the two apart; either way this is not
            // our live app.
            Some(false) => match proc_owner(proc_root, pid) {
                Some(ProcOwner::Missing) | None => Liveness::Dead,
                Some(_) => Liveness::OtherUser,
            },
            // No mechanism could run at all: fall back to /proc, else admit we don't know —
            // NEVER "dead" (that deletes a live app's entry and fires watchdog recovery).
            None => match proc_owner(proc_root, pid) {
                Some(ProcOwner::Ours) => Liveness::Own,
                Some(ProcOwner::Other) => Liveness::OtherUser,
                Some(ProcOwner::Missing) => Liveness::Dead,
                None => Liveness::Unknown,
            },
        }
    }

    /// Run a liveness command with no output; `None` when it could not be spawned.
    fn run_quietly(cmd: &mut Command) -> Option<bool> {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()
            .map(|s| s.success())
    }

    enum ProcOwner {
        Ours,
        Other,
        Missing,
    }

    /// Ownership of `pid` from a procfs mounted at `proc_root`; `None` when there is no
    /// usable procfs (e.g. macOS). `/proc/self` is owned by our effective uid.
    fn proc_owner(proc_root: &Path, pid: u32) -> Option<ProcOwner> {
        use std::os::unix::fs::MetadataExt;
        let me = std::fs::metadata(proc_root.join("self")).ok()?.uid();
        match std::fs::metadata(proc_root.join(pid.to_string())) {
            Ok(meta) if meta.uid() == me => Some(ProcOwner::Ours),
            Ok(_) => Some(ProcOwner::Other),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(ProcOwner::Missing),
            Err(_) => None,
        }
    }
}

#[cfg(not(any(windows, unix)))]
mod imp {
    use super::Liveness;

    pub(super) fn liveness(_pid: u32) -> Liveness {
        Liveness::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_process_is_own_and_live() {
        assert!(is_own_live_process(std::process::id()));
        assert_eq!(liveness(std::process::id()), Liveness::Own);
    }

    #[test]
    fn an_exited_child_is_not_live() {
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
            .args(if cfg!(windows) {
                &["/C", "exit"][..]
            } else {
                &[][..]
            })
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        // `child` still holds its handle, so the PID cannot have been recycled yet.
        assert!(!is_own_live_process(pid));
        assert!(liveness(pid).is_dead(), "{:?}", liveness(pid));
        assert!(!is_own_live_process(0));
    }

    /// PID 4 is the Windows System process: live, but not ours — and, crucially, NOT dead
    /// (its token is unreadable to us; an unreadable token means "exists, unverified", the
    /// same answer an elevated app gets from a non-elevated client — R4-DISC1).
    #[cfg(windows)]
    #[test]
    fn another_accounts_live_process_is_not_ours() {
        assert!(!is_own_live_process(4));
        let l = liveness(4);
        assert!(
            !l.is_dead(),
            "a live process must never read as dead: {l:?}"
        );
        assert_ne!(l, Liveness::Own);
    }

    /// R4-DISC1: with no `kill` binary at all (NixOS/Guix/minimal containers), a live PID
    /// must still read as live — via the shell builtin, then `/proc`, else "unknown" — and
    /// never as dead.
    #[cfg(unix)]
    #[test]
    fn a_missing_kill_binary_never_makes_a_live_pid_dead() {
        use std::path::Path;
        let me = std::process::id();
        let no_kill = &["/nonexistent/victauri/kill"][..];
        let no_sh = &["/nonexistent/victauri/sh"][..];
        let no_proc = Path::new("/nonexistent/victauri/proc");

        // Shell builtin fallback.
        assert_eq!(
            imp::liveness_with(me, no_kill, &["/bin/sh"], no_proc),
            Liveness::Own
        );
        // procfs fallback (Linux).
        if Path::new("/proc/self").exists() {
            assert_eq!(
                imp::liveness_with(me, no_kill, no_sh, Path::new("/proc")),
                Liveness::Own
            );
        }
        // Nothing works: unknown, not dead.
        let l = imp::liveness_with(me, no_kill, no_sh, no_proc);
        assert_eq!(l, Liveness::Unknown);
        assert!(!l.is_dead());
    }

    #[cfg(unix)]
    #[test]
    fn out_of_range_pids_are_never_signalled() {
        // `kill -0 -1` would "succeed" (every process we may signal).
        assert!(liveness(u32::MAX).is_dead());
        assert!(liveness(0).is_dead());
    }
}
