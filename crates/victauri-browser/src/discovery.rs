//! Discovery-directory writer for the browser native host.
//!
//! Mirrors `victauri-plugin`'s discovery pattern so an MCP client can auto-discover
//! the host's port and auth token instead of scraping them from the process log
//! (audit B5). Files live under `<temp>/victauri/<pid>/` with user-only permissions
//! (Unix `0o600` files / `0o700` dir; Windows `icacls` current-user-only).
//!
//! Layout (identical shape to the plugin):
//!   `<temp>/victauri/<pid>/port`           — the bound port, as text
//!   `<temp>/victauri/<pid>/token`          — the Bearer token (only when auth is on)
//!   `<temp>/victauri/<pid>/metadata.json`  — pid, port, mode, version, `started_at`

use std::path::{Path, PathBuf};

/// Per-process discovery directory: `<temp>/victauri/<pid>`.
#[must_use]
pub fn discovery_dir() -> PathBuf {
    std::env::temp_dir()
        .join("victauri")
        .join(std::process::id().to_string())
}

/// Restrict a file or directory to current-user-only access on Windows via `icacls`.
#[cfg(windows)]
#[allow(unsafe_code)]
fn current_windows_username() -> Option<String> {
    use windows::Win32::System::WindowsProgramming::GetUserNameW;
    use windows::core::PWSTR;

    let mut buffer = [0_u16; 257];
    let mut len = buffer.len() as u32;
    // SAFETY: `buffer` is writable for `len` UTF-16 code units and remains alive
    // for the duration of the call. `GetUserNameW` writes at most that capacity.
    unsafe {
        GetUserNameW(Some(PWSTR(buffer.as_mut_ptr())), &raw mut len).ok()?;
    }
    let end = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(len as usize);
    String::from_utf16(&buffer[..end])
        .ok()
        .filter(|name| !name.is_empty())
}

/// NUL-terminated UTF-16 encoding of a path for the Win32 `*W` APIs.
#[cfg(windows)]
fn to_wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// A standalone, owned copy of the current process user's SID.
#[cfg(windows)]
struct OwnedSid(Vec<u8>);

#[cfg(windows)]
impl OwnedSid {
    fn as_psid(&self) -> windows::Win32::Security::PSID {
        windows::Win32::Security::PSID(self.0.as_ptr() as *mut core::ffi::c_void)
    }
}

/// Copy the SID from a token-information class into an owned buffer. Used for `TokenUser`
/// (the account SID) and `TokenOwner` (the SID owning objects this process creates). Both
/// `TOKEN_USER` (`.User.Sid`) and `TOKEN_OWNER` (`.Owner`) lead with the `PSID` at offset 0.
#[cfg(windows)]
#[allow(unsafe_code)]
fn token_sid(class: windows::Win32::Security::TOKEN_INFORMATION_CLASS) -> Option<OwnedSid> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{GetLengthSid, GetTokenInformation, PSID, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    struct TokenGuard(HANDLE);
    impl Drop for TokenGuard {
        fn drop(&mut self) {
            // SAFETY: handle from `OpenProcessToken`, closed exactly once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    let mut token = HANDLE::default();
    // SAFETY: pseudo-handle from `GetCurrentProcess`; `token` owns a real handle on success,
    // closed once by `TokenGuard`.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token).ok()? };
    let _guard = TokenGuard(token);

    let mut len = 0_u32;
    // SAFETY: size probe; null buffer makes the call report the required size in `len`.
    unsafe {
        let _ = GetTokenInformation(token, class, None, 0, &raw mut len);
    }
    if len == 0 {
        return None;
    }
    let mut buf = vec![0_u8; len as usize];
    // SAFETY: `buf` is writable for `len` bytes; on success it holds the requested struct.
    unsafe {
        GetTokenInformation(
            token,
            class,
            Some(buf.as_mut_ptr().cast::<core::ffi::c_void>()),
            len,
            &raw mut len,
        )
        .ok()?;
    }
    // SAFETY: both `TOKEN_USER` and `TOKEN_OWNER` lead with the `PSID` at offset 0.
    let sid_ptr = unsafe { *buf.as_ptr().cast::<PSID>() };
    // SAFETY: `sid_ptr` points to a valid SID within `buf`.
    let sid_len = unsafe { GetLengthSid(sid_ptr) };
    if sid_len == 0 {
        return None;
    }
    let mut sid = vec![0_u8; sid_len as usize];
    // SAFETY: `sid_ptr` is valid for `sid_len` bytes; `sid` has the capacity.
    unsafe {
        core::ptr::copy_nonoverlapping(sid_ptr.0.cast::<u8>(), sid.as_mut_ptr(), sid_len as usize);
    }
    Some(OwnedSid(sid))
}

/// SIDs that legitimately own a directory this process creates: the token USER and the
/// token's default OWNER. Identical for a normal user; an **elevated** admin token's default
/// owner is `BUILTIN\Administrators`, so objects it creates are owned by that group — accept
/// either, or the ownership check would reject every directory we create under elevation.
#[cfg(windows)]
fn acceptable_owner_sids() -> Vec<OwnedSid> {
    use windows::Win32::Security::{TokenOwner, TokenUser};
    [TokenUser, TokenOwner]
        .into_iter()
        .filter_map(token_sid)
        .collect()
}

/// True iff `path` exists and its owner SID is one this process would create objects as
/// (its token user, or — under elevation — its token's default owner group). The Windows
/// counterpart to the Unix uid check — refuses an attacker-pre-planted dir.
#[cfg(windows)]
#[allow(unsafe_code)]
fn dir_owned_by_current_user(path: &Path) -> bool {
    use windows::Win32::Foundation::{ERROR_SUCCESS, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows::Win32::Security::{
        EqualSid, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    };
    use windows::core::PCWSTR;

    let acceptable = acceptable_owner_sids();
    if acceptable.is_empty() {
        return false;
    }
    let wide = to_wide(path);
    let mut owner = PSID::default();
    let mut psd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `wide` is NUL-terminated; OWNER info only; `psd` is OS-allocated, freed below.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&raw mut owner),
            None,
            None,
            None,
            &raw mut psd,
        )
    };
    if rc != ERROR_SUCCESS {
        return false;
    }
    // SAFETY: `owner` and each `sid` are valid SIDs for the comparison.
    let owned = acceptable
        .iter()
        .any(|sid| unsafe { EqualSid(owner, sid.as_psid()).is_ok() });
    // SAFETY: `psd` was allocated by `GetNamedSecurityInfoW`; freed exactly once.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(psd.0)));
    }
    owned
}

/// Replace `path`'s DACL with a PROTECTED, owner-only DACL so NO inherited or pre-planted
/// explicit ACE for any other principal (e.g. `BUILTIN\Guests`) survives. True on success.
#[cfg(windows)]
#[allow(unsafe_code)]
fn apply_owner_only_dacl(path: &Path) -> bool {
    use windows::Win32::Foundation::{ERROR_SUCCESS, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW,
        SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
    };
    use windows::Win32::Security::{
        ACE_FLAGS, ACL, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    };
    use windows::core::PWSTR;

    use windows::Win32::Security::TokenUser;

    const GENERIC_ALL_RIGHTS: u32 = 0x1000_0000;
    const SUB_CONTAINERS_AND_OBJECTS_INHERIT: u32 = 0x3;

    // Grant the token USER full control (retained even when the dir is owned by the
    // Administrators group under elevation).
    let Some(me) = token_sid(TokenUser) else {
        return false;
    };

    let explicit = EXPLICIT_ACCESS_W {
        grfAccessPermissions: GENERIC_ALL_RIGHTS,
        grfAccessMode: SET_ACCESS,
        grfInheritance: ACE_FLAGS(SUB_CONTAINERS_AND_OBJECTS_INHERIT),
        Trustee: TRUSTEE_W {
            pMultipleTrustee: core::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: PWSTR(me.as_psid().0.cast::<u16>()),
        },
    };

    let mut new_acl: *mut ACL = core::ptr::null_mut();
    // SAFETY: one explicit entry, no prior ACL; `new_acl` is LocalAlloc'd, freed below.
    let rc = unsafe { SetEntriesInAclW(Some(&[explicit]), None, &raw mut new_acl) };
    if rc != ERROR_SUCCESS || new_acl.is_null() {
        return false;
    }

    let mut wide = to_wide(path);
    // SAFETY: `wide` is a NUL-terminated mutable path; `new_acl` is valid; PROTECTED strips
    // inheritance and every other explicit ACE.
    let set_rc = unsafe {
        SetNamedSecurityInfoW(
            PWSTR(wide.as_mut_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(new_acl),
            None,
        )
    };
    // SAFETY: `new_acl` from `SetEntriesInAclW`; freed exactly once.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(new_acl.cast::<core::ffi::c_void>())));
    }
    set_rc == ERROR_SUCCESS
}

/// Best-effort `icacls` fallback, used only if the Win32 DACL replacement fails.
#[cfg(windows)]
fn icacls_restrict_to_current_user(path: &Path) -> bool {
    let Some(username) = current_windows_username() else {
        return false;
    };
    let path_str = path.to_string_lossy();
    std::process::Command::new("icacls")
        .args([
            &*path_str,
            "/inheritance:r",
            "/remove",
            "*S-1-1-0",
            "*S-1-5-32-545",
            "*S-1-5-11",
            "/grant:r",
            &format!("{username}:F"),
            "/q",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Lock `path` to owner-only access: robust PROTECTED owner-only DACL first, `icacls`
/// fallback only if that fails. Fail-closed.
#[cfg(windows)]
fn restrict_to_current_user(path: &Path) -> bool {
    if apply_owner_only_dacl(path) {
        return true;
    }
    tracing::warn!(
        "owner-only DACL apply failed for {}; falling back to icacls",
        path.display()
    );
    icacls_restrict_to_current_user(path)
}

#[cfg(not(windows))]
fn restrict_to_current_user(_path: &Path) -> bool {
    true
}

#[cfg(unix)]
fn current_euid() -> Option<u32> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_PROBE: AtomicU64 = AtomicU64::new(0);
    for _ in 0..16 {
        let sequence = NEXT_PROBE.fetch_add(1, Ordering::Relaxed);
        let probe = std::env::temp_dir().join(format!(
            ".victauri_browser_uidprobe_{}_{}",
            std::process::id(),
            sequence
        ));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&probe)
            .ok();
        if let Some(file) = file {
            let uid = file.metadata().ok().map(|m| m.uid());
            drop(file);
            let _ = std::fs::remove_file(probe);
            if uid.is_some() {
                return uid;
            }
        }
    }
    None
}

/// Create or tighten a Unix discovery directory without trusting a planted path.
#[cfg(unix)]
fn ensure_unix_private_dir(path: &Path) -> bool {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    let Some(euid) = current_euid() else {
        return false;
    };
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if !meta.file_type().is_dir() || meta.uid() != euid {
                return false;
            }
            if std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).is_err() {
                return false;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            if builder.create(path).is_err() {
                return false;
            }
        }
        Err(_) => return false,
    }
    unix_private_dir_is_trusted(path)
}

#[cfg(unix)]
fn unix_private_dir_is_trusted(path: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let Some(euid) = current_euid() else {
        return false;
    };
    std::fs::symlink_metadata(path).is_ok_and(|meta| {
        meta.file_type().is_dir() && meta.uid() == euid && (meta.permissions().mode() & 0o077) == 0
    })
}

/// Create the discovery root and process dir only when both paths are trusted.
fn ensure_dir() -> Option<PathBuf> {
    let dir = discovery_dir();
    #[cfg(unix)]
    {
        let root = dir.parent()?;
        if !ensure_unix_private_dir(root) || !ensure_unix_private_dir(&dir) {
            tracing::warn!("refusing untrusted discovery path {}", dir.display());
            return None;
        }
    }
    #[cfg(not(unix))]
    {
        if std::fs::create_dir_all(&dir).is_err() {
            return None;
        }
        // Refuse a discovery dir we don't own (an attacker who pre-created it on a shared
        // TEMP would be its owner). Mirrors the Unix uid check; defeats PID-preplant before
        // any token is written. A dir we just created we own, so the normal path passes.
        #[cfg(windows)]
        if !dir_owned_by_current_user(&dir) {
            tracing::warn!(
                "refusing discovery dir not owned by current user: {}",
                dir.display()
            );
            let _ = std::fs::remove_dir_all(&dir);
            return None;
        }
        if !restrict_to_current_user(&dir) {
            let _ = std::fs::remove_dir_all(&dir);
            return None;
        }
    }
    Some(dir)
}

/// Write `content` to `<discovery_dir>/<name>` as a fresh, user-only file. Uses
/// exclusive (`create_new` / `O_EXCL`) creation so a pre-planted file OR symlink
/// is refused rather than written through, and sets `0600` at creation on Unix so
/// there is no window where the file exists with default-umask permissions.
fn write_locked(dir: &Path, name: &str, content: &str) {
    let path = dir.join(name);
    // Clear any stale/pre-planted entry (symlink-aware) so the exclusive create
    // succeeds for our own fresh file; a symlink racing in afterwards is refused.
    if std::fs::symlink_metadata(&path).is_ok() {
        let _ = std::fs::remove_file(&path);
    }
    #[cfg(unix)]
    let result = {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut f| f.write_all(content.as_bytes()))
    };
    #[cfg(not(unix))]
    let result = {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut f| f.write_all(content.as_bytes()))
    };
    if let Err(e) = result {
        tracing::debug!("could not write discovery file {name}: {e}");
        return;
    }
    if !restrict_to_current_user(&path) {
        let _ = std::fs::remove_file(&path);
        tracing::warn!("could not restrict discovery file {}", path.display());
    }
}

/// Write the discovery files for this running host.
///
/// `token` is written only when `Some` (auth enabled); the `metadata.json` always
/// records `auth_required` so a client knows whether to expect a token file.
pub fn write(port: u16, token: Option<&str>) {
    let Some(dir) = ensure_dir() else {
        return;
    };

    write_locked(&dir, "port", &port.to_string());

    if let Some(token) = token {
        write_locked(&dir, "token", token);
    }

    let metadata = serde_json::json!({
        "pid": std::process::id(),
        "port": port,
        "mode": "browser",
        "auth_required": token.is_some(),
        "version": env!("CARGO_PKG_VERSION"),
        "started_at": chrono::Utc::now().to_rfc3339(),
    });
    write_locked(&dir, "metadata.json", &metadata.to_string());
}

/// Remove the discovery directory (best-effort, on shutdown).
pub fn remove() {
    let dir = discovery_dir();
    #[cfg(unix)]
    {
        let Some(root) = dir.parent() else {
            return;
        };
        if !unix_private_dir_is_trusted(root) || !unix_private_dir_is_trusted(&dir) {
            return;
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: the discovery dir is keyed on the process PID, so all tests in this
    // module share one directory. They therefore run as a single sequential test
    // to avoid racing on that shared state.
    #[test]
    fn write_read_and_auth_modes() {
        // --- auth enabled: port + token + metadata all present ---
        remove();
        write(7474, Some("test-token-abc"));

        let dir = discovery_dir();
        assert_eq!(std::fs::read_to_string(dir.join("port")).unwrap(), "7474");
        assert_eq!(
            std::fs::read_to_string(dir.join("token")).unwrap(),
            "test-token-abc"
        );

        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("metadata.json")).unwrap())
                .unwrap();
        assert_eq!(meta["port"], 7474);
        assert_eq!(meta["mode"], "browser");
        assert_eq!(meta["auth_required"], true);
        assert_eq!(meta["pid"], std::process::id());

        // --- auth disabled: no token file, auth_required = false ---
        remove();
        write(7475, None);

        assert_eq!(std::fs::read_to_string(dir.join("port")).unwrap(), "7475");
        assert!(!dir.join("token").exists());
        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("metadata.json")).unwrap())
                .unwrap();
        assert_eq!(meta["auth_required"], false);

        // --- remove() clears the directory ---
        remove();
        assert!(!dir.exists());
    }

    // Round-4 audit blocker #4: a pre-planted explicit ACE for an arbitrary principal
    // (the auditor used BUILTIN\Guests) must NOT survive the discovery-dir hardening.
    #[cfg(windows)]
    #[test]
    fn owner_only_dacl_removes_pre_planted_guests_ace() {
        use std::process::Command;
        let dir = std::env::temp_dir()
            .join("victauri_browser_acl_test")
            .join(format!("p{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test dir");

        assert!(
            dir_owned_by_current_user(&dir),
            "a freshly created dir must be recognized as owned by this process"
        );

        let path_str = dir.to_string_lossy().to_string();
        let Ok(grant) = Command::new("icacls")
            .args([path_str.as_str(), "/grant", "*S-1-5-32-546:(OI)(CI)F", "/q"])
            .output()
        else {
            let _ = std::fs::remove_dir_all(&dir);
            return; // icacls unavailable — skip
        };
        if !grant.status.success() {
            let _ = std::fs::remove_dir_all(&dir);
            return; // could not plant — skip
        }

        let before = Command::new("icacls")
            .arg(path_str.as_str())
            .output()
            .expect("icacls read");
        assert!(
            String::from_utf8_lossy(&before.stdout).contains("Guests"),
            "pre-condition: planted Guests ACE should be visible"
        );

        assert!(
            apply_owner_only_dacl(&dir),
            "apply_owner_only_dacl must succeed on a directory we own"
        );

        let after = Command::new("icacls")
            .arg(path_str.as_str())
            .output()
            .expect("icacls read");
        let after_s = String::from_utf8_lossy(&after.stdout);
        assert!(
            !after_s.contains("Guests"),
            "pre-planted Guests ACE must NOT survive the owner-only DACL, got:\n{after_s}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn private_dir_refuses_symlink_without_chmodding_target() {
        use std::os::unix::fs::PermissionsExt;

        let base = std::env::temp_dir().join(format!(
            "victauri-browser-discovery-test-{}",
            uuid::Uuid::new_v4()
        ));
        let target = base.join("target");
        let link = base.join("link");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(!ensure_unix_private_dir(&link));
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "symlink target permissions must be untouched");
        let _ = std::fs::remove_dir_all(base);
    }
}
