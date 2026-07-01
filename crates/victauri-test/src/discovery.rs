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

/// Discover one unambiguous live server, keeping its port and token together.
pub fn scan_discovery_dirs_for_connection() -> Option<(u16, Option<String>)> {
    unique_connection(&find_live_servers())
}

/// Return a discovery token only when exactly one live entry advertises `port`.
pub fn scan_discovery_dirs_for_token_on_port(port: u16) -> Option<String> {
    unique_token_for_port(&find_live_servers(), port)
}

/// Return the live discovery entry belonging to one spawned process.
pub fn scan_discovery_dir_for_pid(pid: u32) -> Option<(u16, Option<String>)> {
    let servers = find_live_servers();
    let mut matches = servers.iter().filter(|server| server.pid == pid);
    let server = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some((server.port, server.token.clone()))
}

/// Explicit configured port, if valid.
pub fn configured_port() -> Option<u16> {
    std::env::var("VICTAURI_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|port| *port != 0)
}

fn configured_token() -> Option<String> {
    std::env::var("VICTAURI_AUTH_TOKEN")
        .ok()
        .filter(|token| !token.trim().is_empty())
}

/// Resolve a connection without ever pairing a token with a different port.
pub fn resolve_connection() -> (u16, Option<String>) {
    let explicit_port = configured_port();
    let explicit_token = configured_token();

    if let Some(port) = explicit_port {
        let token = explicit_token.or_else(|| scan_discovery_dirs_for_token_on_port(port));
        return (port, token);
    }

    // A configured token is an explicit credential for the default endpoint. Do not
    // send it to an arbitrary auto-discovered server.
    if let Some(token) = explicit_token {
        return (7373, Some(token));
    }

    scan_discovery_dirs_for_connection().unwrap_or((7373, None))
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
        // A dead owner process is stale even if the advertised (shared default) port is
        // reachable — that reachability is a different live app, not this one.
        if !is_process_alive(pid) {
            stale.push((pid, port));
            continue;
        }
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
    pid: u32,
    port: u16,
    token: Option<String>,
}

fn unique_connection(servers: &[DiscoveredServer]) -> Option<(u16, Option<String>)> {
    if servers.len() != 1 {
        return None;
    }
    Some((servers[0].port, servers[0].token.clone()))
}

fn unique_token_for_port(servers: &[DiscoveredServer], port: u16) -> Option<String> {
    let mut matching = servers.iter().filter(|server| server.port == port);
    let server = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    server.token.clone()
}

fn find_live_servers() -> Vec<DiscoveredServer> {
    find_live_servers_in(&victauri_base_dir(), is_process_alive, port_is_reachable)
}

/// Testable core of [`find_live_servers`]: scan `base`, keep only entries whose OWNING
/// PROCESS is alive and whose advertised port is reachable, and prune the rest.
///
/// Liveness is gated on the owning **pid**, not just port reachability — because every
/// Victauri app registers on the SAME default port (7373). A crashed app's stale entry
/// still advertises 7373, and a *different* live app now holding 7373 makes that port
/// probe succeed, so a reachability-only check keeps the dead entry and pairs its stale
/// token with the live server → 401. Checking the pid distinguishes them: a dead owner
/// means the entry is stale even when the shared port answers. (The MCP-bridge discovery
/// path already did this; this brings the test/CLI path in line.)
fn find_live_servers_in(
    base: &std::path::Path,
    is_alive: impl Fn(u32) -> bool,
    is_reachable: impl Fn(u16) -> bool,
) -> Vec<DiscoveredServer> {
    if !dir_is_trusted(base) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(base) else {
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
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        // Only trust dirs we own — never read a token from, or delete, a dir a
        // local attacker could have planted (audit #15).
        if !dir_is_trusted(&path) {
            continue;
        }
        // A dead owner process means this entry is stale — even if its advertised port is
        // reachable, that is a DIFFERENT live app now holding the shared default port, not
        // this one. Reachability can't tell them apart, so gate on the pid.
        if !is_alive(pid) {
            let _ = std::fs::remove_dir_all(&path);
            continue;
        }
        let port_path = path.join("port");
        let Ok(port_str) = std::fs::read_to_string(&port_path) else {
            continue;
        };
        let Ok(port) = port_str.trim().parse::<u16>() else {
            continue;
        };
        // Live owner but unreachable port = mid-rebuild / not listening yet — not usable now.
        if !is_reachable(port) {
            let _ = std::fs::remove_dir_all(&path);
            continue;
        }
        let token = std::fs::read_to_string(path.join("token"))
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        servers.push(DiscoveredServer { pid, port, token });
    }
    servers
}

fn port_is_reachable(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(100),
    )
    .is_ok()
}

/// Cross-platform "is this pid a live process?" — mirrors the audited helper in the
/// MCP-bridge discovery path (`victauri-cli::bridge`). Used to prune discovery entries
/// whose owning app has exited even when a different app holds the same shared port.
#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
}

#[cfg(not(windows))]
fn is_process_alive(pid: u32) -> bool {
    // `kill -0` sends no signal but succeeds iff the process exists and is signalable by us
    // (discovery entries are our own user's processes). Portable across Linux and macOS.
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
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

    #[test]
    fn connection_selection_keeps_port_and_token_together() {
        let servers = vec![DiscoveredServer {
            pid: 10,
            port: 7374,
            token: Some("token-b".to_string()),
        }];
        assert_eq!(
            unique_connection(&servers),
            Some((7374, Some("token-b".to_string())))
        );
    }

    #[test]
    fn token_selection_never_crosses_or_ambiguously_matches_ports() {
        let servers = vec![
            DiscoveredServer {
                pid: 10,
                port: 7373,
                token: Some("token-a".to_string()),
            },
            DiscoveredServer {
                pid: 11,
                port: 7374,
                token: Some("token-b".to_string()),
            },
        ];
        assert_eq!(
            unique_token_for_port(&servers, 7374).as_deref(),
            Some("token-b")
        );
        assert_eq!(unique_token_for_port(&servers, 7999), None);

        let duplicate = vec![
            DiscoveredServer {
                pid: 12,
                port: 7373,
                token: Some("old-token".to_string()),
            },
            DiscoveredServer {
                pid: 13,
                port: 7373,
                token: Some("new-token".to_string()),
            },
        ];
        assert_eq!(unique_token_for_port(&duplicate, 7373), None);
    }

    #[test]
    fn dead_pid_sharing_a_live_port_is_pruned_so_selection_stays_unambiguous() {
        // The shared-default-port collision (the real 401 cause): a crashed app (pid 1) and a
        // live app (pid 2) both advertise 7373. Reachability alone can't tell them apart — the
        // live app answers for both — so a reachability-only scan keeps BOTH and pairs the stale
        // token with the live server => `unique_token_for_port` returns None => 401. Gating on
        // the owning pid prunes the dead entry so selection is unambiguous again.
        let base = tempfile::tempdir().unwrap();
        for (pid, token) in [("1", "STALE-TOKEN"), ("2", "LIVE-TOKEN")] {
            let dir = base.path().join(pid);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("port"), "7373").unwrap();
            std::fs::write(dir.join("token"), token).unwrap();
        }
        // pid 2 alive, pid 1 dead; the shared port answers for both.
        let servers = find_live_servers_in(base.path(), |pid| pid == 2, |_port| true);

        assert_eq!(
            servers.len(),
            1,
            "the dead-pid entry must be pruned despite the shared port answering"
        );
        assert_eq!(servers[0].pid, 2);
        assert_eq!(servers[0].token.as_deref(), Some("LIVE-TOKEN"));
        assert!(
            !base.path().join("1").exists(),
            "the dead-pid discovery dir should be cleaned up"
        );
        assert!(base.path().join("2").exists(), "the live dir stays");
        // Selection is unambiguous now — the exact fix for the 401.
        assert_eq!(
            unique_connection(&servers),
            Some((7373, Some("LIVE-TOKEN".to_string())))
        );
        assert_eq!(
            unique_token_for_port(&servers, 7373).as_deref(),
            Some("LIVE-TOKEN")
        );
    }

    #[test]
    fn live_owner_with_unreachable_port_is_pruned() {
        // A live owning process whose port isn't listening yet (mid-rebuild) is not usable now.
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("4242");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("port"), "7373").unwrap();
        std::fs::write(dir.join("token"), "tok").unwrap();
        let servers = find_live_servers_in(base.path(), |_pid| true, |_port| false);
        assert!(
            servers.is_empty(),
            "unreachable port => not a usable server"
        );
        assert!(
            !base.path().join("4242").exists(),
            "the unreachable dir is pruned"
        );
    }
}
