//! Process liveness for discovery: is `pid` a live process owned by the current user?
//!
//! Discovery trusts a `<temp>/victauri/<pid>/` entry only while its PID is alive, so this
//! check decides where a Bearer token is sent. It must be exact (PID 12 is not PID 123),
//! scoped to our own user (another user's process that inherited a stale PID must not make
//! an entry look live), and cheap — the CLI bridge polls it every 1.5 s. The previous
//! Windows check spawned `tasklist` and substring-matched its output: it failed all three
//! (measured 0.5 s per call on an idle machine, up to 40 s filtered by user under load).
//!
//! Same-user PID reuse remains a documented residual: discovery liveness is PID-based.

/// Whether `pid` is a live process owned by the current user.
#[doc(hidden)]
#[must_use]
pub fn is_own_live_process(pid: u32) -> bool {
    imp::is_own_live_process(pid)
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use std::ffi::c_void;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, STILL_ACTIVE};
    use windows_sys::Win32::Security::{EqualSid, GetTokenInformation, TOKEN_QUERY, TokenUser};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

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

    pub(super) fn is_own_live_process(pid: u32) -> bool {
        if pid == 0 {
            return false;
        }
        // SAFETY: plain FFI call; a null return (no such process, or not ours to query)
        // is handled below.
        let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if raw.is_null() {
            return false;
        }
        let process = Handle(raw);
        let mut code = 0u32;
        // SAFETY: `process.0` is a valid handle; `code` is a valid out-pointer.
        if unsafe { GetExitCodeProcess(process.0, &mut code) } == 0 || code != STILL_ACTIVE as u32 {
            return false;
        }
        // SAFETY: the pseudo-handle of the current process needs no closing.
        let me = unsafe { GetCurrentProcess() };
        let (Some(theirs), Some(ours)) = (token_user(process.0), token_user(me)) else {
            return false;
        };
        // SAFETY: both SIDs point into live TOKEN_USER buffers held above.
        unsafe { EqualSid(sid(&theirs), sid(&ours)) != 0 }
    }
}

#[cfg(unix)]
mod imp {
    /// `kill -0` succeeds only for a live process we may signal: our own (unless root).
    pub(super) fn is_own_live_process(pid: u32) -> bool {
        pid != 0
            && std::process::Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
    }
}

#[cfg(not(any(windows, unix)))]
mod imp {
    pub(super) fn is_own_live_process(_pid: u32) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_process_is_own_and_live() {
        assert!(is_own_live_process(std::process::id()));
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
        assert!(!is_own_live_process(0));
    }

    /// PID 4 is the Windows System process: live, but not ours.
    #[cfg(windows)]
    #[test]
    fn another_accounts_live_process_is_not_ours() {
        assert!(!is_own_live_process(4));
    }
}
