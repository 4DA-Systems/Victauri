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

#[cfg(windows)]
fn restrict_to_current_user(path: &Path) -> bool {
    let Some(username) = current_windows_username() else {
        return false;
    };
    let path_str = path.to_string_lossy();
    // Remove the common world/group principals (Everyone, BUILTIN\Users, Authenticated Users)
    // before granting owner-only: `/inheritance:r` strips only INHERITED ACEs and `/grant:r`
    // replaces only the owner's, so a pre-planted explicit `Everyone:(F)` would otherwise survive.
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
