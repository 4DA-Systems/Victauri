//! Per-process server discovery for CI parallelism.
//!
//! Victauri servers write discovery files to `<temp>/victauri/<pid>/` with
//! port, token, and metadata. This module scans those directories and returns
//! the live server(s). Stale directories from dead processes are cleaned up
//! by checking TCP connectivity on the advertised port.

use std::path::PathBuf;

fn victauri_base_dir() -> PathBuf {
    std::env::temp_dir().join("victauri")
}

/// Whether a discovery directory is safe to trust (audit #15). On Unix the temp
/// root (e.g. `/tmp`) is world-writable, so an attacker can plant a fake `<pid>`
/// dir pointing at a server they control to steal the token / forge results. We
/// trust a dir only if it is a real directory (not a symlink), owned by the current
/// effective user, and not group/other-writable. On Windows the temp dir is already
/// per-user, and the writer restricts ACLs via `icacls`, so no extra check is needed.
#[cfg(unix)]
fn dir_is_trusted(path: &std::path::Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.file_type().is_dir() {
        return false; // reject symlinks / non-dirs
    }
    let Some(euid) = current_euid() else {
        return false; // can't establish our uid -> don't trust
    };
    meta.uid() == euid && (meta.permissions().mode() & 0o022) == 0
}

/// Determine the current effective uid without `unsafe` code (this crate
/// `#![forbid(unsafe_code)]`): exclusively create a file and read back its owner uid.
#[cfg(unix)]
fn current_euid() -> Option<u32> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_PROBE: AtomicU64 = AtomicU64::new(0);
    for _ in 0..16 {
        let sequence = NEXT_PROBE.fetch_add(1, Ordering::Relaxed);
        let probe = std::env::temp_dir().join(format!(
            ".victauri_uidprobe_{}_{}",
            std::process::id(),
            sequence
        ));
        if let Some(uid) = uid_from_exclusive_probe(&probe) {
            return Some(uid);
        }
    }
    None
}

/// Create a UID probe without following a pre-planted symlink in the shared temp dir.
#[cfg(unix)]
fn uid_from_exclusive_probe(probe: &std::path::Path) -> Option<u32> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(probe)
        .ok()?;
    let uid = file.metadata().ok().map(|m| m.uid());
    drop(file);
    let _ = std::fs::remove_file(probe);
    uid
}

#[cfg(not(unix))]
fn dir_is_trusted(_path: &std::path::Path) -> bool {
    true
}

/// Scan per-process discovery directories and return the port of a live server.
///
/// Returns `None` if no live server is found, or if multiple are found (ambiguous).
pub fn scan_discovery_dirs_for_port() -> Option<u16> {
    let servers = find_live_servers();
    if servers.len() == 1 {
        return Some(servers[0].port);
    }
    None
}

/// Scan per-process discovery directories and return the token of a live server.
pub fn scan_discovery_dirs_for_token() -> Option<String> {
    let servers = find_live_servers();
    if servers.len() == 1 {
        return servers[0].token.clone();
    }
    None
}

/// Non-destructive classification of the discovery directory, used to turn a
/// bare "connection refused" into an actionable diagnosis.
#[derive(Debug, Clone)]
pub enum DiscoveryStatus {
    /// At least one Victauri server is reachable.
    Live,
    /// Discovery directories exist but none are reachable — the app process(es)
    /// advertised these ports then exited (crashed, closed, or is rebuilding).
    Stale {
        /// `(pid, port)` pairs from the stale discovery directories.
        stale: Vec<(u32, u16)>,
    },
    /// No discovery directories at all — the app never started, or it is a
    /// release build (Victauri is gated to debug builds).
    None,
}

impl DiscoveryStatus {
    /// A human-readable, actionable hint for this status, or `None` when live.
    #[must_use]
    pub fn hint(&self) -> Option<String> {
        match self {
            Self::Live => None,
            Self::Stale { stale } => {
                let detail = stale
                    .iter()
                    .map(|(pid, port)| format!("PID {pid} on port {port}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                Some(format!(
                    "A Victauri app was running ({detail}) but its server is now unreachable — \
                     the app process has exited. It most likely crashed, was closed, or its \
                     backend is mid-rebuild. Victauri runs inside the app, so it cannot report \
                     build/compile status itself: check your build or dev-server terminal, then \
                     relaunch the app and retry."
                ))
            }
            Self::None => Some(
                "No Victauri server discovery files were found. Either the app is not running, \
                 or it is a release build (Victauri is enabled only in debug builds via \
                 #[cfg(debug_assertions)]). Start the app in a debug/dev build and retry."
                    .to_string(),
            ),
        }
    }
}

/// Classify the discovery directory **without** deleting stale entries.
///
/// Call this before [`scan_discovery_dirs_for_port`] (which cleans up dead dirs)
/// when you want to explain *why* a connection failed.
#[must_use]
pub fn diagnose_discovery() -> DiscoveryStatus {
    let base = victauri_base_dir();
    if !dir_is_trusted(&base) {
        return DiscoveryStatus::None;
    }
    let Ok(entries) = std::fs::read_dir(&base) else {
        return DiscoveryStatus::None;
    };

    let mut stale = Vec::new();
    let mut any_dir = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(pid) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if !dir_is_trusted(&path) {
            continue;
        }
        let Ok(port_str) = std::fs::read_to_string(path.join("port")) else {
            continue;
        };
        let Ok(port) = port_str.trim().parse::<u16>() else {
            continue;
        };
        any_dir = true;
        if std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            std::time::Duration::from_millis(100),
        )
        .is_ok()
        {
            return DiscoveryStatus::Live;
        }
        stale.push((pid, port));
    }

    if any_dir {
        DiscoveryStatus::Stale { stale }
    } else {
        DiscoveryStatus::None
    }
}

struct DiscoveredServer {
    port: u16,
    token: Option<String>,
}

fn find_live_servers() -> Vec<DiscoveredServer> {
    let base = victauri_base_dir();
    if !dir_is_trusted(&base) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Vec::new();
    };

    let mut servers = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(pid_str) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if pid_str.parse::<u32>().is_err() {
            continue;
        }
        // Only trust dirs we own — never read a token from, or delete, a dir a
        // local attacker could have planted (audit #15).
        if !dir_is_trusted(&path) {
            continue;
        }
        let port_path = path.join("port");
        let Ok(port_str) = std::fs::read_to_string(&port_path) else {
            continue;
        };
        let Ok(port) = port_str.trim().parse::<u16>() else {
            continue;
        };
        // Check if the port is reachable — if not, the server is dead
        if std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            std::time::Duration::from_millis(100),
        )
        .is_err()
        {
            let _ = std::fs::remove_dir_all(&path);
            continue;
        }
        let token = std::fs::read_to_string(path.join("token"))
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        servers.push(DiscoveredServer { port, token });
    }
    servers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn uid_probe_refuses_preplanted_symlink_without_clobbering_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let probe = dir.path().join("probe");
        std::fs::write(&target, "must-survive").unwrap();
        std::os::unix::fs::symlink(&target, &probe).unwrap();

        assert_eq!(uid_from_exclusive_probe(&probe), None);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "must-survive");
    }

    #[test]
    fn live_status_has_no_hint() {
        assert!(DiscoveryStatus::Live.hint().is_none());
    }

    #[test]
    fn stale_status_names_pid_and_port() {
        let hint = DiscoveryStatus::Stale {
            stale: vec![(1234, 7374)],
        }
        .hint()
        .expect("stale has a hint");
        assert!(hint.contains("1234"), "hint names the PID: {hint}");
        assert!(hint.contains("7374"), "hint names the port: {hint}");
        assert!(
            hint.contains("crashed") || hint.contains("rebuild"),
            "hint explains the likely cause: {hint}"
        );
    }

    #[test]
    fn none_status_mentions_debug_build() {
        let hint = DiscoveryStatus::None.hint().expect("none has a hint");
        assert!(
            hint.contains("debug") || hint.contains("not running"),
            "hint explains app-not-running / release-build: {hint}"
        );
    }
}
