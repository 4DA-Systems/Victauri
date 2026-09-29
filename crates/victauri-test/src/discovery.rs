//! Per-process server discovery for CI parallelism.
//!
//! Victauri servers write discovery files to `<temp>/victauri/<pid>/` with
//! port, token, and metadata. This module scans those directories and returns
//! the live server(s). A directory is deleted only when its owning process is
//! definitely dead — never merely because its port did not answer a probe (a
//! live-but-busy app writes its entry once, so deleting it would make the app
//! undiscoverable for the rest of its life).

use std::path::PathBuf;

/// The discovery roots the plugin may have written to, most specific first (mirrors the
/// plugin's `discovery_root`). On Unix the root is per-user — `$XDG_RUNTIME_DIR/victauri` when
/// that directory is private to us, else `<temp>/victauri-<euid>` (a shared `/tmp/victauri`
/// could be pre-created by another user, blocking discovery) — and the legacy
/// `<temp>/victauri` is still read, subject to the same ownership check, for pre-0.9 plugins.
/// Other platforms use `<temp>/victauri` (a per-user temp dir).
#[cfg(not(test))]
fn discovery_roots() -> Vec<PathBuf> {
    real_discovery_roots()
}

/// Unit tests never read (or prune) the machine's REAL discovery directories — a developer's
/// running apps live there. Every discovery path in this crate's unit tests resolves against
/// one private, empty, process-wide root instead.
#[cfg(test)]
fn discovery_roots() -> Vec<PathBuf> {
    static ROOT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    vec![
        ROOT.get_or_init(|| tempfile::tempdir().expect("isolated discovery root"))
            .path()
            .to_path_buf(),
    ]
}

/// Where a running app's auth token lives, for error messages — the same roots
/// [`discovery_roots`] scans, in the same order.
#[cfg(unix)]
pub const TOKEN_LOCATIONS: &str = "$XDG_RUNTIME_DIR/victauri/<pid>/token (when that \
     directory is private to you), else <temp>/victauri-<uid>/<pid>/token";
/// Where a running app's auth token lives, for error messages.
#[cfg(not(unix))]
pub const TOKEN_LOCATIONS: &str = r"%TEMP%\victauri\<pid>\token";

#[cfg_attr(all(test, not(unix)), allow(dead_code))]
fn real_discovery_roots() -> Vec<PathBuf> {
    let legacy = std::env::temp_dir().join("victauri");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let mut roots = Vec::new();
        if let Some(euid) = current_euid() {
            if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .filter(|dir| dir.is_absolute())
                .filter(|dir| {
                    std::fs::symlink_metadata(dir).is_ok_and(|m| {
                        m.file_type().is_dir()
                            && m.uid() == euid
                            && m.permissions().mode() & 0o077 == 0
                    })
                })
            {
                roots.push(runtime.join("victauri"));
            }
            roots.push(std::env::temp_dir().join(format!("victauri-{euid}")));
        }
        roots.push(legacy);
        roots
    }
    #[cfg(not(unix))]
    {
        vec![legacy]
    }
}

/// Whether a discovery directory is safe to trust (audit #15). On Unix the temp
/// root (e.g. `/tmp`) is world-writable, so an attacker can plant a fake `<pid>`
/// dir pointing at a server they control to steal the token / forge results. We
/// trust a dir only if it is a real directory (not a symlink), owned by the current
/// effective user, and not group/other-writable. (Windows has its own owner check below.)
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
    for _ in 0..16 {
        // Unpredictable name (R4-DISC2): a guessable `<pid>_<seq>` name in the shared temp
        // dir let another user pre-create every probe path and deny us our own uid.
        let probe = std::env::temp_dir().join(format!(
            ".victauri_uidprobe_{}",
            uuid::Uuid::new_v4().simple()
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

/// Windows: a real directory (a symlink or junction is not `is_dir()` under
/// `symlink_metadata`) OWNED by the current user (see
/// [`crate::process::dir_owned_by_current_user`]). `%TEMP%` is normally per-user, but it can
/// be shared (an app launched from MSYS2 uses `C:\msys64\tmp`), where another user could
/// plant `victauri\<live pid>\` entries pointing at a port they control (R5B-WINDISC1).
#[cfg(windows)]
fn dir_is_trusted(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
        && crate::process::dir_owned_by_current_user(path)
}

#[cfg(not(any(unix, windows)))]
fn dir_is_trusted(_path: &std::path::Path) -> bool {
    true
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
        .and_then(|value| value.trim().parse::<u16>().ok())
        .filter(|port| *port != 0)
}

fn configured_token() -> Option<String> {
    std::env::var("VICTAURI_AUTH_TOKEN")
        .ok()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

/// The app selector from `VICTAURI_APP` (a Tauri bundle identifier), if set.
pub fn configured_app() -> Option<String> {
    std::env::var("VICTAURI_APP")
        .ok()
        .map(|app| app.trim().to_string())
        .filter(|app| !app.is_empty())
}

/// Resolve a connection without ever pairing a token with a different port, falling back
/// to the default port with NO token when discovery cannot pick one app. Prefer
/// [`try_resolve_connection`], which explains an ambiguous or refused resolution.
pub fn resolve_connection() -> (u16, Option<String>) {
    try_resolve_connection(None).unwrap_or((DEFAULT_PORT, None))
}

/// The port a Victauri app binds when nothing else is configured.
const DEFAULT_PORT: u16 = 7373;

/// Resolve the endpoint to connect to from `VICTAURI_PORT` / `VICTAURI_AUTH_TOKEN` /
/// `VICTAURI_APP` (or `app`, which overrides `VICTAURI_APP`) and the live discovery entries.
///
/// # Errors
///
/// A human-readable explanation when several apps match and nothing selects one, when the
/// selected app is not running, or when an explicit token matches no running app (it is
/// then sent nowhere rather than to whatever process holds the default port).
pub fn try_resolve_connection(app: Option<&str>) -> Result<(u16, Option<String>), String> {
    let app = app.map(str::to_string).or_else(configured_app);
    let port = configured_port();
    let token = configured_token();
    // Only scan when the answer depends on discovery (an explicit port + token with no app
    // selector does not; a selector is checked against that port's entry — R5B-PORTAPP1).
    let servers = if port.is_some() && token.is_some() && app.is_none() {
        Vec::new()
    } else {
        find_live_servers()
    };
    resolve_from(port, token, app.as_deref(), &servers)
}

/// Pure core of [`try_resolve_connection`].
fn resolve_from(
    explicit_port: Option<u16>,
    explicit_token: Option<String>,
    app: Option<&str>,
    servers: &[DiscoveredServer],
) -> Result<(u16, Option<String>), String> {
    // An explicit port is the caller naming the endpoint: pair it with the explicit token,
    // else with the token of the one live entry on exactly that port. An app selector set as
    // well must AGREE with that port's app (R5B-PORTAPP1) — it used to be silently ignored, so
    // the client drove whatever app held the port. (The client re-checks `/info` on connect,
    // which also covers a port with no discovery entry.)
    if let Some(port) = explicit_port {
        if let (Some(app), Some(entry)) = (app, unique_server_on_port(servers, port))
            && (entry.identifier.is_some() || entry.product_name.is_some())
            && !entry.matches_app(app)
        {
            return Err(port_app_mismatch(port, &entry.label(), app));
        }
        let token = explicit_token.or_else(|| unique_token_for_port(servers, port));
        return Ok((port, token));
    }

    let candidates: Vec<&DiscoveredServer> = servers
        .iter()
        .filter(|server| app.is_none_or(|app| server.matches_app(app)))
        .collect();

    // An explicit token WITHOUT an explicit port (R4-TOK1): it used to be sent to whatever
    // listened on 7373 — on a shared machine, a squatter there received it and could replay
    // it against the real app. Send it only to the unique live, trusted app whose own
    // discovery token IS that token.
    if let Some(token) = explicit_token {
        let owners: Vec<&&DiscoveredServer> = candidates
            .iter()
            .filter(|server| server.token.as_deref() == Some(token.as_str()))
            .collect();
        return match owners.as_slice() {
            [owner] => Ok((owner.port, Some(token))),
            [] => Err(
                "VICTAURI_AUTH_TOKEN is set, but no running Victauri app's discovery entry \
                 carries that token, so it was not sent anywhere (without VICTAURI_PORT it \
                 would have gone to whatever process holds the default port). Set \
                 VICTAURI_PORT to the app's port to use this token explicitly, or unset \
                 VICTAURI_AUTH_TOKEN to use the discovered one."
                    .to_string(),
            ),
            many => Err(ambiguity_message(many.iter().map(|s| **s))),
        };
    }

    match (candidates.as_slice(), app) {
        ([one], _) => Ok((one.port, one.token.clone())),
        ([], Some(app)) => {
            let running = if servers.is_empty() {
                " No Victauri app is running.".to_string()
            } else {
                let labels: Vec<String> = servers.iter().map(DiscoveredServer::label).collect();
                format!(" Running apps:\n  {}", labels.join("\n  "))
            };
            Err(format!(
                "No running Victauri app matches the app selector '{app}' (VICTAURI_APP / \
                 --app: an exact bundle identifier or product name).{running}"
            ))
        }
        // Nothing discovered: the legacy default endpoint, with no token.
        ([], None) => Ok((DEFAULT_PORT, None)),
        (many, _) => Err(ambiguity_message(many.iter().copied())),
    }
}

/// `VICTAURI_PORT` and an app selector name different apps.
pub fn port_app_mismatch(port: u16, found: &str, app: &str) -> String {
    format!(
        "VICTAURI_PORT={port} is app {}, but the app selector (--app / VICTAURI_APP) is '{}'. \
         Unset one of them, or point VICTAURI_PORT at that app's port.",
        crate::terminal::single_line(found),
        crate::terminal::single_line(app)
    )
}

/// "Several apps match" — names each as `identifier (port N, pid P)` and how to pick one
/// (the same guidance `victauri bridge` gives).
fn ambiguity_message<'a>(servers: impl Iterator<Item = &'a DiscoveredServer>) -> String {
    let labels: Vec<String> = servers.map(DiscoveredServer::label).collect();
    format!(
        "Multiple Victauri apps are running:\n  {}\nSelect one with `--app <bundle-identifier>` \
         (victauri CLI) or the VICTAURI_APP env var, or pin the port with VICTAURI_PORT.",
        labels.join("\n  ")
    )
}

/// Re-resolve the endpoint of a previously discovered client, pinned to its app.
///
/// Explicit configuration (`VICTAURI_PORT` / `VICTAURI_AUTH_TOKEN`) is honored exactly
/// as [`try_resolve_connection`] does — the caller named the endpoint. Otherwise only a
/// live discovery entry whose app `identifier` equals `expected_identifier` is accepted;
/// when the identity is unknown (`None`, e.g. a plugin too old to report one) the single
/// live server is accepted as before. Returns `None` — never the default port — when
/// nothing (or more than one server) matches, so a restarted client cannot silently bind
/// to a different app that happens to hold the shared default port.
pub fn resolve_rediscovery(expected_identifier: Option<&str>) -> Option<(u16, Option<String>)> {
    if configured_port().is_some() || configured_token().is_some() {
        return try_resolve_connection(expected_identifier).ok();
    }
    select_for_identity(&find_live_servers(), expected_identifier)
}

/// The app identifier advertised in discovery metadata by the single live server on
/// `port`, if exactly one such server exists and it recorded an identifier.
pub fn identifier_for_port(port: u16) -> Option<String> {
    let servers = find_live_servers();
    let mut matching = servers.iter().filter(|server| server.port == port);
    let server = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    server.identifier.clone()
}

fn select_for_identity(
    servers: &[DiscoveredServer],
    expected_identifier: Option<&str>,
) -> Option<(u16, Option<String>)> {
    let Some(expected) = expected_identifier else {
        return unique_connection(servers);
    };
    let mut matching = servers
        .iter()
        .filter(|server| server.identifier.as_deref() == Some(expected));
    let server = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    Some((server.port, server.token.clone()))
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
    let mut stale = Vec::new();
    let mut any_dir = false;
    let entries = discovery_roots()
        .into_iter()
        .filter(|base| dir_is_trusted(base))
        .filter_map(|base| std::fs::read_dir(base).ok())
        .flat_map(Iterator::flatten);
    for entry in entries {
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
        if !crate::process::is_own_live_process(pid) {
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
    /// Tauri app identifier from `metadata.json`, when the plugin recorded one.
    identifier: Option<String>,
    /// Tauri product name from `metadata.json`, when the plugin recorded one.
    product_name: Option<String>,
}

impl DiscoveredServer {
    /// An app selector matches the bundle identifier or the product name EXACTLY (ASCII
    /// case-insensitive, like `victauri bridge --app`) — never a substring, so `com.example`
    /// can't silently bind `com.example.other`.
    fn matches_app(&self, app: &str) -> bool {
        identity_matches(
            self.identifier.as_deref(),
            self.product_name.as_deref(),
            app,
        )
    }

    /// `identifier (port N, pid P)` — the label `victauri bridge` prints, plus the pid.
    fn label(&self) -> String {
        let name = self
            .identifier
            .as_deref()
            .or(self.product_name.as_deref())
            .unwrap_or("<unknown app>");
        format!("{name} (port {}, pid {})", self.port, self.pid)
    }
}

fn unique_connection(servers: &[DiscoveredServer]) -> Option<(u16, Option<String>)> {
    if servers.len() != 1 {
        return None;
    }
    Some((servers[0].port, servers[0].token.clone()))
}

fn unique_token_for_port(servers: &[DiscoveredServer], port: u16) -> Option<String> {
    unique_server_on_port(servers, port)?.token.clone()
}

/// The ONE live entry advertising `port`; `None` when there is none, or several.
fn unique_server_on_port(servers: &[DiscoveredServer], port: u16) -> Option<&DiscoveredServer> {
    let mut matching = servers.iter().filter(|server| server.port == port);
    let server = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    Some(server)
}

/// Whether an app selector names this identity: the bundle identifier or the product name,
/// EXACTLY (ASCII case-insensitive) — the rule discovery, `/info` checks and the CLI bridge share.
pub fn identity_matches(identifier: Option<&str>, product_name: Option<&str>, app: &str) -> bool {
    identifier.is_some_and(|v| v.eq_ignore_ascii_case(app))
        || product_name.is_some_and(|v| v.eq_ignore_ascii_case(app))
}

fn find_live_servers() -> Vec<DiscoveredServer> {
    let mut servers: Vec<DiscoveredServer> = Vec::new();
    for root in discovery_roots() {
        for server in find_live_servers_in(&root, crate::process::liveness, port_is_reachable) {
            // A pid already found under a more specific root counts once.
            if !servers.iter().any(|seen| seen.pid == server.pid) {
                servers.push(server);
            }
        }
    }
    servers
}

/// Testable core of [`find_live_servers`]: scan `base` and keep only entries whose OWNING
/// PROCESS is alive and ours, and whose advertised port is reachable.
///
/// Liveness is gated on the owning **pid**, not just port reachability — because every
/// Victauri app registers on the SAME default port (7373). A crashed app's stale entry
/// still advertises 7373, and a *different* live app now holding 7373 makes that port
/// probe succeed, so a reachability-only check keeps the dead entry and pairs its stale
/// token with the live server → 401. Checking the pid distinguishes them: a dead owner
/// means the entry is stale even when the shared port answers.
///
/// An entry is DELETED only when its owner is definitely dead (R4-DISC1). A live owner
/// whose port did not answer one short probe (busy, still starting, mid-rebuild) is skipped
/// for now but kept — the plugin writes its entry once, so deleting it made a live app
/// undiscoverable for its whole lifetime. An owner whose liveness could not be established
/// (no working `kill`, an unreadable elevated process) is likewise skipped, never deleted.
fn find_live_servers_in(
    base: &std::path::Path,
    liveness: impl Fn(u32) -> crate::process::Liveness,
    is_reachable: impl Fn(u16) -> bool,
) -> Vec<DiscoveredServer> {
    use crate::process::Liveness;

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
        match liveness(pid) {
            Liveness::Own => {}
            // Definitely gone: the entry is stale even if its advertised (shared default)
            // port answers — that is a DIFFERENT live app. Clean it up.
            Liveness::Dead => {
                let _ = std::fs::remove_dir_all(&path);
                continue;
            }
            // Alive but not verifiably ours, or unknowable: never use its token, never
            // delete it.
            _ => continue,
        }
        let Ok(port_str) = std::fs::read_to_string(path.join("port")) else {
            continue;
        };
        let Ok(port) = port_str.trim().parse::<u16>() else {
            continue;
        };
        // Live owner but unreachable port = busy / not listening yet — not usable NOW, but
        // the entry stays for the next scan.
        if !is_reachable(port) {
            continue;
        }
        let token = std::fs::read_to_string(path.join("token"))
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        let metadata = std::fs::read_to_string(path.join("metadata.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
        let field = |key: &str| {
            metadata
                .as_ref()
                .and_then(|meta| meta.get(key))
                .and_then(serde_json::Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        servers.push(DiscoveredServer {
            pid,
            port,
            token,
            identifier: field("identifier"),
            product_name: field("product_name"),
        });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::Liveness;

    /// Audit N6: the per-user root is scanned first; the legacy shared root is last.
    #[cfg(unix)]
    #[test]
    fn discovery_roots_are_per_user_first() {
        let roots = real_discovery_roots();
        let euid = current_euid().unwrap();
        assert!(roots.contains(&std::env::temp_dir().join(format!("victauri-{euid}"))));
        assert_eq!(roots.last(), Some(&std::env::temp_dir().join("victauri")));
        assert_ne!(roots[0], std::env::temp_dir().join("victauri"));
    }

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
            identifier: None,
            product_name: None,
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
                identifier: None,
                product_name: None,
            },
            DiscoveredServer {
                pid: 11,
                port: 7374,
                token: Some("token-b".to_string()),
                identifier: None,
                product_name: None,
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
                identifier: None,
                product_name: None,
            },
            DiscoveredServer {
                pid: 13,
                port: 7373,
                token: Some("new-token".to_string()),
                identifier: None,
                product_name: None,
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
        let servers = find_live_servers_in(
            base.path(),
            |pid| {
                if pid == 2 {
                    Liveness::Own
                } else {
                    Liveness::Dead
                }
            },
            |_port| true,
        );

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

    fn server(pid: u32, port: u16, token: &str, identifier: Option<&str>) -> DiscoveredServer {
        DiscoveredServer {
            pid,
            port,
            token: Some(token.to_string()),
            identifier: identifier.map(str::to_string),
            product_name: None,
        }
    }

    #[test]
    fn rediscovery_pins_the_app_identity() {
        // The restarted app came back on 7374; a DIFFERENT app now holds 7373.
        let servers = vec![
            server(20, 7373, "other-token", Some("com.other.app")),
            server(21, 7374, "mine-token", Some("com.mine.app")),
        ];
        assert_eq!(
            select_for_identity(&servers, Some("com.mine.app")),
            Some((7374, Some("mine-token".to_string())))
        );
    }

    #[test]
    fn rediscovery_never_falls_back_when_the_app_is_absent() {
        // Only a different app is live (on the default port): no match => None, never
        // that app and never a default-port guess.
        let servers = vec![server(20, 7373, "other-token", Some("com.other.app"))];
        assert_eq!(select_for_identity(&servers, Some("com.mine.app")), None);
        assert_eq!(select_for_identity(&[], Some("com.mine.app")), None);
    }

    #[test]
    fn rediscovery_rejects_an_ambiguous_identity_match() {
        let servers = vec![
            server(20, 7373, "a", Some("com.mine.app")),
            server(21, 7374, "b", Some("com.mine.app")),
        ];
        assert_eq!(select_for_identity(&servers, Some("com.mine.app")), None);
    }

    #[test]
    fn rediscovery_without_known_identity_keeps_the_unique_server_rule() {
        let one = vec![server(20, 7375, "t", None)];
        assert_eq!(
            select_for_identity(&one, None),
            Some((7375, Some("t".to_string())))
        );
        let two = vec![server(20, 7375, "t", None), server(21, 7376, "u", None)];
        assert_eq!(select_for_identity(&two, None), None);
    }

    #[test]
    fn scan_reads_the_identifier_from_metadata() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("77");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("port"), "7380").unwrap();
        std::fs::write(dir.join("token"), "tok").unwrap();
        std::fs::write(
            dir.join("metadata.json"),
            r#"{"pid":77,"port":7380,"identifier":"com.meta.app","product_name":"Meta"}"#,
        )
        .unwrap();
        let servers = find_live_servers_in(base.path(), |_| Liveness::Own, |_| true);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].identifier.as_deref(), Some("com.meta.app"));
        assert_eq!(
            select_for_identity(&servers, Some("com.meta.app")),
            Some((7380, Some("tok".to_string())))
        );
    }

    #[test]
    fn live_owner_with_unreachable_port_is_skipped_but_kept() {
        // A live owning process whose port did not answer one short probe (busy, starting,
        // mid-rebuild) is not usable NOW — but its entry must survive: the plugin writes it
        // once, so deleting it made the app undiscoverable for its whole life (R4-DISC1).
        // (This test used to assert the deletion — that was the bug.)
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("4242");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("port"), "7373").unwrap();
        std::fs::write(dir.join("token"), "tok").unwrap();
        let servers = find_live_servers_in(base.path(), |_pid| Liveness::Own, |_port| false);
        assert!(
            servers.is_empty(),
            "unreachable port => not a usable server"
        );
        assert!(
            base.path().join("4242").exists(),
            "a live app's discovery entry must never be deleted"
        );
        // The next scan, once the app answers, finds it.
        let servers = find_live_servers_in(base.path(), |_pid| Liveness::Own, |_port| true);
        assert_eq!(servers.len(), 1);
    }

    #[test]
    fn unverifiable_or_unknown_owners_are_neither_used_nor_deleted() {
        // R4-DISC1: an alive-but-unreadable owner (elevated app on Windows) or an owner whose
        // liveness could not be checked at all (no `kill` binary) must not lose its entry,
        // and must not have its token used.
        let base = tempfile::tempdir().unwrap();
        for pid in ["10", "11", "12"] {
            let dir = base.path().join(pid);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("port"), "7373").unwrap();
            std::fs::write(dir.join("token"), format!("tok-{pid}")).unwrap();
        }
        let servers = find_live_servers_in(
            base.path(),
            |pid| match pid {
                10 => Liveness::Unverified,
                11 => Liveness::Unknown,
                _ => Liveness::OtherUser,
            },
            |_| true,
        );
        assert!(
            servers.is_empty(),
            "no token may be used from these entries"
        );
        for pid in ["10", "11", "12"] {
            assert!(base.path().join(pid).exists(), "entry {pid} must survive");
        }
    }

    fn named(
        pid: u32,
        port: u16,
        token: &str,
        identifier: &str,
        product: &str,
    ) -> DiscoveredServer {
        DiscoveredServer {
            pid,
            port,
            token: Some(token.to_string()),
            identifier: Some(identifier.to_string()),
            product_name: Some(product.to_string()),
        }
    }

    #[test]
    fn an_explicit_token_without_a_port_goes_only_to_the_app_that_owns_it() {
        // R4-TOK1: VICTAURI_AUTH_TOKEN with no VICTAURI_PORT used to resolve to
        // (7373, token) — handing the token to whoever squats the default port.
        let servers = vec![
            named(20, 7373, "squatter-sees-nothing", "com.other.app", "Other"),
            named(21, 7374, "my-token", "com.mine.app", "Mine"),
        ];
        assert_eq!(
            resolve_from(None, Some("my-token".into()), None, &servers),
            Ok((7374, Some("my-token".to_string())))
        );
        // No running app carries it: refuse (never the default port), and say how to fix.
        let err = resolve_from(None, Some("my-token".into()), None, &servers[..1]).unwrap_err();
        assert!(err.contains("VICTAURI_PORT"), "{err}");
        let err = resolve_from(None, Some("my-token".into()), None, &[]).unwrap_err();
        assert!(err.contains("not sent anywhere"), "{err}");
        // Explicit port + explicit token keeps the old behaviour exactly.
        assert_eq!(
            resolve_from(Some(7373), Some("t".into()), None, &servers),
            Ok((7373, Some("t".to_string())))
        );
    }

    #[test]
    fn several_live_apps_are_an_error_naming_each_and_how_to_select() {
        // R4-CLI1: with two apps and no selector, the CLI silently fell back to
        // (7373, no token) and then blamed a 401 on a stale CLI.
        let servers = vec![
            named(20, 7373, "a", "com.a.app", "A"),
            named(21, 7374, "b", "com.b.app", "B"),
        ];
        let err = resolve_from(None, None, None, &servers).unwrap_err();
        assert!(err.contains("com.a.app (port 7373"), "{err}");
        assert!(err.contains("com.b.app (port 7374"), "{err}");
        assert!(
            err.contains("VICTAURI_APP") && err.contains("--app"),
            "{err}"
        );
        assert!(err.contains("VICTAURI_PORT"), "{err}");
        // A selector picks one, by exact identifier or exact product name.
        assert_eq!(
            resolve_from(None, None, Some("com.b.app"), &servers),
            Ok((7374, Some("b".to_string())))
        );
        assert_eq!(
            resolve_from(None, None, Some("A"), &servers),
            Ok((7373, Some("a".to_string())))
        );
        // Never a substring match.
        let err = resolve_from(None, None, Some("com.b"), &servers).unwrap_err();
        assert!(err.contains("No running Victauri app matches"), "{err}");
        // One app, or none, behaves as before.
        assert_eq!(
            resolve_from(None, None, None, &servers[..1]),
            Ok((7373, Some("a".to_string())))
        );
        assert_eq!(resolve_from(None, None, None, &[]), Ok((7373, None)));
    }

    /// Hand `path` to `NT AUTHORITY\SYSTEM` — the stand-in for "another user" (needs an
    /// elevated test run; `false` when that is not possible, and the caller skips).
    #[cfg(windows)]
    fn give_to_system(path: &std::path::Path) -> bool {
        std::process::Command::new("icacls")
            .arg(path)
            .args(["/setowner", "*S-1-5-18", "/q"])
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// R5B-WINDISC1: on a shared TEMP another user can plant `victauri\<live pid>\…`; the
    /// readers must refuse a root or entry directory this user does not own.
    #[cfg(windows)]
    #[test]
    fn windows_discovery_refuses_directories_owned_by_another_user() {
        let write_entry = |base: &std::path::Path, pid: &str| {
            let dir = base.join(pid);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("port"), "7373").unwrap();
            std::fs::write(dir.join("token"), format!("tok-{pid}")).unwrap();
            dir
        };
        // A planted entry in a root we own.
        let base = tempfile::tempdir().unwrap();
        write_entry(base.path(), "10");
        let planted = write_entry(base.path(), "11");
        if !give_to_system(&planted) {
            eprintln!("skipped: cannot change a directory's owner (not elevated)");
            return;
        }
        let servers = find_live_servers_in(base.path(), |_| Liveness::Own, |_| true);
        let pids: Vec<u32> = servers.iter().map(|s| s.pid).collect();
        assert_eq!(pids, [10], "the planted entry's token must never be used");
        assert!(
            planted.exists(),
            "a foreign directory is never deleted either"
        );

        // A planted ROOT: nothing under it is trusted, even entries we own.
        let root = tempfile::tempdir().unwrap();
        write_entry(root.path(), "12");
        assert!(give_to_system(root.path()));
        assert!(find_live_servers_in(root.path(), |_| Liveness::Own, |_| true).is_empty());
    }

    /// R5B-PORTAPP1: an explicit port and an app selector must agree.
    #[test]
    fn an_explicit_port_and_a_disagreeing_app_selector_are_refused() {
        let servers = vec![
            named(20, 7373, "a", "com.a.app", "A"),
            named(21, 7374, "b", "com.b.app", "B"),
        ];
        let err = resolve_from(Some(7373), None, Some("com.b.app"), &servers).unwrap_err();
        assert!(
            err.contains("VICTAURI_PORT=7373") && err.contains("com.a.app"),
            "{err}"
        );
        assert!(err.contains("com.b.app"), "{err}");
        // With an explicit token too.
        assert!(resolve_from(Some(7373), Some("t".into()), Some("com.b.app"), &servers).is_err());
        // Agreeing (by identifier or product name, any case) is fine.
        assert_eq!(
            resolve_from(Some(7374), None, Some("b"), &servers),
            Ok((7374, Some("b".to_string())))
        );
        // No discovery entry on the port: left to the client's `/info` check.
        assert_eq!(
            resolve_from(Some(7999), None, Some("com.b.app"), &servers),
            Ok((7999, None))
        );
    }

    #[test]
    fn duplicate_identifiers_are_ambiguous_not_first_wins() {
        let servers = vec![
            named(20, 7373, "a", "com.dup.app", "Dup"),
            named(21, 7374, "b", "com.dup.app", "Dup"),
        ];
        let err = resolve_from(None, None, Some("com.dup.app"), &servers).unwrap_err();
        assert!(err.contains("pid 20") && err.contains("pid 21"), "{err}");
    }
}
