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
fn restrict_to_current_user(path: &Path) {
    let Ok(username) = std::env::var("USERNAME") else {
        return;
    };
    let path_str = path.to_string_lossy();
    let _ = std::process::Command::new("icacls")
        .args([
            &*path_str,
            "/inheritance:r",
            "/grant:r",
            &format!("{username}:F"),
            "/q",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn restrict_to_current_user(_path: &Path) {}

/// Create the discovery dir (if needed) and lock it down to the current user.
///
/// Removes a SYMLINK or regular FILE squatting at the path first (a planted
/// symlink on a shared `/tmp` could redirect our writes); an existing real
/// directory is left in place (idempotent across the per-file writes below).
fn ensure_dir() -> PathBuf {
    let dir = discovery_dir();
    if let Ok(meta) = std::fs::symlink_metadata(&dir)
        && (meta.file_type().is_symlink() || meta.is_file())
    {
        let _ = std::fs::remove_file(&dir);
    }
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    restrict_to_current_user(&dir);
    dir
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
    let result = std::fs::write(&path, content);
    if let Err(e) = result {
        tracing::debug!("could not write discovery file {name}: {e}");
        return;
    }
    restrict_to_current_user(&path);
}

/// Write the discovery files for this running host.
///
/// `token` is written only when `Some` (auth enabled); the `metadata.json` always
/// records `auth_required` so a client knows whether to expect a token file.
pub fn write(port: u16, token: Option<&str>) {
    let dir = ensure_dir();

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
    let _ = std::fs::remove_dir_all(discovery_dir());
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
}
