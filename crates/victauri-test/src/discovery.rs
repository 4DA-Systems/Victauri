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
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    meta.uid() == euid && meta.permissions().mode() & 0o022 == 0
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

struct DiscoveredServer {
    port: u16,
    token: Option<String>,
}

fn find_live_servers() -> Vec<DiscoveredServer> {
    let base = victauri_base_dir();
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
