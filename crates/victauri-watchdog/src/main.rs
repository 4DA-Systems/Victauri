//! Watchdog process that monitors and restarts the Victauri MCP server if it becomes unresponsive.

mod process;

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Minimum poll interval (seconds). A zero/sub-second interval would let a
/// permanently-failing health check spin into a busy loop, so we floor it.
const MIN_INTERVAL_SECS: u64 = 1;
/// Default poll interval (seconds) when unset or unparseable.
const DEFAULT_INTERVAL_SECS: u64 = 5;
/// Minimum consecutive failures before a recovery action fires. A value of 0
/// would mean "recover on the very first (or every) poll", so we floor it at 1.
const MIN_MAX_FAILURES: u32 = 1;
/// Default consecutive-failure threshold when unset or unparseable.
const DEFAULT_MAX_FAILURES: u32 = 3;
/// Hard timeout for a recovery command. A hung recovery (e.g. a command that
/// blocks forever) must not wedge the watchdog itself, so the child is killed
/// after this many seconds and the failure is reported.
const RECOVERY_TIMEOUT_SECS: u64 = 60;

/// Port polled when none is configured and discovery finds no single live app.
const DEFAULT_PORT: u16 = 7373;

struct Config {
    /// Port to poll: the explicit one (`VICTAURI_PORT` / positional `PORT`) or
    /// [`DEFAULT_PORT`]. When not explicit, `main` resolves it via discovery.
    port: u16,
    /// `true` when the port was set explicitly — discovery is then skipped
    /// entirely, preserving the pre-discovery behaviour exactly.
    port_explicit: bool,
    /// App selector for discovery (`--app <id>` / `VICTAURI_APP`): matches the
    /// Tauri bundle identifier or product name in the discovery `metadata.json`.
    app: Option<String>,
    interval: Duration,
    max_failures: u32,
    on_failure_cmd: Option<String>,
}

impl Config {
    fn from_env() -> Self {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let (arg_port, arg_app) = parse_args(&args);
        let explicit_port = std::env::var("VICTAURI_PORT")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .or(arg_port);
        Self {
            port: explicit_port.unwrap_or(DEFAULT_PORT),
            port_explicit: explicit_port.is_some(),
            app: arg_app.or_else(|| {
                std::env::var("VICTAURI_APP")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            }),
            interval: clamp_interval(std::env::var("VICTAURI_INTERVAL").ok().as_deref()),
            max_failures: clamp_max_failures(
                std::env::var("VICTAURI_MAX_FAILURES").ok().as_deref(),
            ),
            on_failure_cmd: std::env::var("VICTAURI_ON_FAILURE").ok(),
        }
    }
}

/// Parse CLI args: an optional positional `PORT` and `--app <id>` / `--app=<id>`.
fn parse_args(args: &[String]) -> (Option<u16>, Option<String>) {
    let mut port = None;
    let mut app = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--app" {
            app = iter.next().cloned();
        } else if let Some(v) = arg.strip_prefix("--app=") {
            app = Some(v.to_string());
        } else if port.is_none()
            && let Ok(p) = arg.parse::<u16>()
        {
            port = Some(p);
        }
    }
    (port, app.filter(|a| !a.is_empty()))
}

// ── Discovery ────────────────────────────────────────────────────────────────
//
// The plugin writes `<root>/<pid>/port` (see `discovery_roots`; + `metadata.json` with the app
// `identifier` / `product_name`) and may land on 7374+ when 7373 is taken, so a
// fixed port can watch the wrong app or nothing at all. `/health` needs no auth,
// so only the port is read — never the token.

/// Which discovery entries the watchdog is willing to follow.
///
/// The watchdog exists to *report* a crash. If it followed "whatever Victauri app is
/// running" after the watched one died, a second app would turn a crash into a silent
/// green. So once an app is resolved, its identity is PINNED and only entries with that
/// identity are ever followed afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Selector {
    /// Nothing resolved yet and no `--app`: any single live app (the first resolution).
    Any,
    /// `--app` / `VICTAURI_APP`: matches the bundle `identifier` or the `product_name`.
    App(String),
    /// Pinned after resolution: the app's bundle identifier (or, for an entry written by a
    /// plugin without an identifier, its product name). Only this identity is followed.
    Pinned(String),
    /// Pinned to one process whose discovery entry carried no identity at all: it can never
    /// be re-identified after a restart, so the watchdog never follows anything else.
    Pid(u32),
}

impl Selector {
    fn matches(&self, pid: u32, meta: &serde_json::Value) -> bool {
        let field = |k: &str| {
            meta.get(k)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        };
        // Identity comparison is EXACT but ASCII case-insensitive — identical to the CLI
        // bridge's `matches_app` and victauri-test's discovery (R5-WD1). A case-only mismatch
        // must never read as "app down": that fires the recovery command against a healthy app.
        let same =
            |value: Option<&str>, want: &str| value.is_some_and(|v| v.eq_ignore_ascii_case(want));
        match self {
            Self::Any => true,
            Self::App(want) => same(field("identifier"), want) || same(field("product_name"), want),
            Self::Pinned(id) => match field("identifier") {
                Some(identifier) => identifier.eq_ignore_ascii_case(id),
                None => same(field("product_name"), id),
            },
            Self::Pid(p) => *p == pid,
        }
    }

    /// The selector to use from now on, once `app` has been resolved.
    fn pin(&self, app: &DiscoveredApp) -> Self {
        match self {
            Self::Pinned(_) => self.clone(),
            _ => app
                .identity
                .clone()
                .map_or(Self::Pid(app.pid), Self::Pinned),
        }
    }
}

/// One live discovery entry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscoveredApp {
    pid: u32,
    port: u16,
    /// Bundle identifier, else product name, from `metadata.json` (if any).
    identity: Option<String>,
}

/// Outcome of scanning the discovery directory.
#[derive(Debug, PartialEq, Eq)]
enum Discovery {
    /// Exactly one live app matched.
    Found(DiscoveredApp),
    /// No live app matched.
    None,
    /// Several live apps matched and the selector does not disambiguate them.
    Ambiguous(Vec<(u32, u16)>),
}

/// The discovery roots the plugin may have written to, most specific first (mirrors the
/// plugin's `discovery_root`). On Unix the roots are per-user — `$XDG_RUNTIME_DIR/victauri`
/// when that directory is private to us, then `<temp>/victauri-<euid>` (a shared
/// `/tmp/victauri` could be pre-created by another user, blocking discovery), then the home
/// fallback `$XDG_STATE_HOME/victauri` / `$HOME/.local/state/victauri` (where the plugin
/// registers when `<temp>/victauri-<euid>` was taken over) — and the legacy `<temp>/victauri`
/// is still read, subject to the same ownership check, for pre-0.9 plugins. Other platforms
/// use `<temp>/victauri` (a per-user temp dir; Windows readers verify its ownership).
fn discovery_roots() -> Vec<PathBuf> {
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
            // The plugin's fallback when `<temp>/victauri-<euid>` was taken over by another
            // user (R5B-LINDISC1) — scanned after it, before the legacy root.
            roots.extend(home_state_root(
                std::env::var_os("XDG_STATE_HOME"),
                std::env::var_os("HOME"),
            ));
        }
        roots.push(legacy);
        roots
    }
    #[cfg(not(unix))]
    {
        vec![legacy]
    }
}

/// The plugin's per-user fallback discovery root under the home directory:
/// `$XDG_STATE_HOME/victauri`, else `$HOME/.local/state/victauri` (each only when absolute).
/// The plugin registers there when `<temp>/victauri-<euid>` is untrusted — e.g. another local
/// user pre-created it, and sticky `/tmp` stops us deleting it (R5B-LINDISC1).
#[cfg(unix)]
fn home_state_root(
    xdg_state_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    let absolute =
        |p: std::ffi::OsString| Some(std::path::PathBuf::from(p)).filter(|p| p.is_absolute());
    xdg_state_home
        .and_then(absolute)
        .or_else(|| {
            home.and_then(absolute)
                .map(|h| h.join(".local").join("state"))
        })
        .map(|state| state.join("victauri"))
}

/// [`discover_in`] over every discovery root; a pid found under several roots counts once.
fn discover_in_roots(
    roots: &[PathBuf],
    selector: &Selector,
    is_alive: impl Fn(u32) -> bool,
) -> Discovery {
    let mut found: Vec<DiscoveredApp> = Vec::new();
    let mut apps: Vec<(u32, u16)> = Vec::new();
    for root in roots {
        match discover_in(root, selector, &is_alive) {
            Discovery::Found(app) => {
                if !apps.iter().any(|(pid, _)| *pid == app.pid) {
                    apps.push((app.pid, app.port));
                    found.push(app);
                }
            }
            Discovery::Ambiguous(more) => {
                for app in more {
                    if !apps.iter().any(|(pid, _)| *pid == app.0) {
                        apps.push(app);
                    }
                }
            }
            Discovery::None => {}
        }
    }
    match (apps.len(), found.len()) {
        (0, _) => Discovery::None,
        (1, 1) => Discovery::Found(found.remove(0)),
        _ => {
            apps.sort_unstable();
            Discovery::Ambiguous(apps)
        }
    }
}

/// Scan `base` for live Victauri apps (owning pid alive) that match `selector`.
///
/// Both the base directory and every entry directory must pass [`dir_is_trusted`]; an
/// untrusted base yields [`Discovery::None`] (on Unix `/tmp` is world-writable, so a planted
/// `victauri/` tree must not be able to steer the watchdog).
fn discover_in(base: &Path, selector: &Selector, is_alive: impl Fn(u32) -> bool) -> Discovery {
    if !dir_is_trusted(base) {
        return Discovery::None;
    }
    let Ok(entries) = std::fs::read_dir(base) else {
        return Discovery::None;
    };
    let mut found: Vec<DiscoveredApp> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(pid) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if !dir_is_trusted(&path) {
            continue;
        }
        let Some(port) = std::fs::read_to_string(path.join("port"))
            .ok()
            .and_then(|s| s.trim().parse::<u16>().ok())
            .filter(|p| *p != 0)
        else {
            continue;
        };
        let meta: serde_json::Value = std::fs::read_to_string(path.join("metadata.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if !selector.matches(pid, &meta) {
            continue;
        }
        // Liveness last: it spawns a process on Windows, so only pay it for matches.
        if !is_alive(pid) {
            continue;
        }
        let identity = ["identifier", "product_name"].iter().find_map(|k| {
            meta.get(*k)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        });
        found.push(DiscoveredApp {
            pid,
            port,
            identity,
        });
    }
    match found.len() {
        0 => Discovery::None,
        1 => Discovery::Found(found.remove(0)),
        _ => {
            let mut apps: Vec<(u32, u16)> = found.iter().map(|a| (a.pid, a.port)).collect();
            apps.sort_unstable();
            Discovery::Ambiguous(apps)
        }
    }
}

/// On Unix the temp root is world-writable: only trust a real (non-symlink) directory
/// that is OWNED by the current effective user and is not group/other-writable. Permission
/// bits alone are not enough — another local user can create a `0700` directory of their
/// own. Mirrors `victauri-test`'s `discovery::dir_is_trusted`.
#[cfg(unix)]
fn dir_is_trusted(path: &Path) -> bool {
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

/// Determine the current effective uid without `unsafe` code: exclusively create a file
/// and read back its owner uid (same approach as `victauri-test`). Cached for the process.
#[cfg(unix)]
fn current_euid() -> Option<u32> {
    use std::sync::OnceLock;

    static EUID: OnceLock<Option<u32>> = OnceLock::new();
    *EUID.get_or_init(|| {
        for _ in 0..16 {
            // Unpredictable name (R4-DISC2): a guessable `<pid>_<seq>` name in the shared
            // temp dir let another user pre-create every probe path and deny us our uid.
            let probe = std::env::temp_dir().join(format!(
                ".victauri_watchdog_uidprobe_{}",
                unpredictable_suffix()
            ));
            if let Some(uid) = uid_from_exclusive_probe(&probe) {
                return Some(uid);
            }
        }
        None
    })
}

/// 128 bits another local user cannot predict, without a new dependency: two `SipHash`
/// outputs keyed by `RandomState` (seeded from the OS RNG per process), mixed with the time.
#[cfg(unix)]
fn unpredictable_suffix() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let word = |salt: u64| {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(nanos);
        h.write_u64(salt);
        h.write_u32(std::process::id());
        h.finish()
    };
    format!("{:016x}{:016x}", word(1), word(2))
}

/// Create a UID probe without following a pre-planted symlink in the shared temp dir
/// (`create_new` = `O_EXCL`, which refuses any existing path including a symlink).
#[cfg(unix)]
fn uid_from_exclusive_probe(probe: &Path) -> Option<u32> {
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

/// Windows: a real directory (a symlink or junction is not `is_dir()` under `symlink_metadata`)
/// OWNED by the current user — the token user, the token's default owner, or
/// `BUILTIN\Administrators` when this token is a member (the plugin writer's rule). `%TEMP%` is
/// normally per-user, but a shared one (`C:\msys64\tmp`) let another user plant an entry that
/// pointed the watchdog at a port they control (R5B-WINDISC1).
#[cfg(windows)]
fn dir_is_trusted(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
        && process::dir_owned_by_current_user(path)
}

#[cfg(not(any(unix, windows)))]
fn dir_is_trusted(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// Whether a discovery entry's owner may be the app to watch: anything not known to be gone.
/// The watchdog only reads the entry's PORT (never its token), so an owner whose liveness
/// could not be checked — no `kill` binary on NixOS/Guix/minimal images, an elevated app
/// whose token is unreadable — is still followed (R4-DISC1); `/health` decides from there.
fn may_be_alive(pid: u32) -> bool {
    !is_gone(process::liveness(pid))
}

/// Whether a watched process has definitely exited (or its PID now belongs to another
/// account, i.e. was recycled). "Could not tell" is NOT gone: before R4-DISC1 a missing
/// `/bin/kill` made every PID read dead, and the watchdog fired its recovery command on a
/// healthy app. (Exact PID, own user; see `process.rs` — the old Windows check
/// substring-matched `tasklist` output, so a crashed app whose PID was a prefix of a live one
/// looked alive and was never restarted.)
fn is_gone(liveness: process::Liveness) -> bool {
    matches!(
        liveness,
        process::Liveness::Dead | process::Liveness::OtherUser
    )
}

/// Resolve the app to watch from discovery, logging an ambiguous outcome. Returns `None`
/// when discovery yields no single matching app.
fn discover_app(selector: &Selector) -> Option<DiscoveredApp> {
    match discover_in_roots(&discovery_roots(), selector, may_be_alive) {
        Discovery::Found(app) => Some(app),
        Discovery::None => None,
        Discovery::Ambiguous(apps) => {
            tracing::warn!(
                ?apps,
                ?selector,
                "Several matching Victauri apps are running — set --app <identifier> \
                 (or VICTAURI_APP) or VICTAURI_PORT to choose one"
            );
            None
        }
    }
}

/// What the watchdog polls.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    port: u16,
    /// The watched process, when resolved by discovery. A dead pid is a failure even if
    /// something else now answers on the port (that would be a different app).
    pid: Option<u32>,
}

/// Initial target before the first poll.
///
/// - explicit port → that port, no discovery (pre-discovery behaviour);
/// - discovered app → its port + pid, and the selector is pinned to its identity;
/// - `--app` set but not found → `None`: the app is reported DOWN. Falling back to the
///   default port would watch whatever unrelated app holds 7373 and hide the outage;
/// - no `--app`, nothing found → the default port (legacy fallback, identity unknown).
fn initial_target(
    config: &Config,
    selector: &mut Selector,
    discovered: Option<DiscoveredApp>,
) -> Option<Target> {
    if config.port_explicit {
        return Some(Target {
            port: config.port,
            pid: None,
        });
    }
    if let Some(app) = discovered {
        *selector = selector.pin(&app);
        return Some(Target {
            port: app.port,
            pid: Some(app.pid),
        });
    }
    if config.app.is_some() {
        None
    } else {
        Some(Target {
            port: DEFAULT_PORT,
            pid: None,
        })
    }
}

/// While failing, re-resolve through discovery — ONLY among entries matching the (pinned)
/// selector — and follow the watched app if it restarted on a new pid/port. Returns the
/// new target when it changed.
fn follow_target(
    current: Option<&Target>,
    selector: &mut Selector,
    discovered: Option<DiscoveredApp>,
) -> Option<Target> {
    let app = discovered?;
    let next = Target {
        port: app.port,
        pid: Some(app.pid),
    };
    if current == Some(&next) {
        return None;
    }
    *selector = selector.pin(&app);
    Some(next)
}

/// Parse `VICTAURI_INTERVAL` (seconds) and clamp it up to `MIN_INTERVAL_SECS`.
///
/// Unset or unparseable falls back to `DEFAULT_INTERVAL_SECS`. A configured
/// value below the floor (including 0) is clamped UP to the floor and emits a
/// warning — a zero interval must never cause a busy loop.
fn clamp_interval(raw: Option<&str>) -> Duration {
    let secs = match raw {
        Some(s) => match s.trim().parse::<u64>() {
            Ok(v) => v,
            Err(_) => DEFAULT_INTERVAL_SECS,
        },
        None => DEFAULT_INTERVAL_SECS,
    };
    if secs < MIN_INTERVAL_SECS {
        tracing::warn!(
            configured = secs,
            floor = MIN_INTERVAL_SECS,
            "VICTAURI_INTERVAL below minimum — clamping up to floor to avoid a busy loop"
        );
        Duration::from_secs(MIN_INTERVAL_SECS)
    } else {
        Duration::from_secs(secs)
    }
}

/// Parse `VICTAURI_MAX_FAILURES` and clamp it up to `MIN_MAX_FAILURES`.
///
/// Unset or unparseable falls back to `DEFAULT_MAX_FAILURES`. A configured
/// value below 1 (i.e. 0) would mean "recover immediately / every poll" and is
/// clamped UP to 1 with a warning.
fn clamp_max_failures(raw: Option<&str>) -> u32 {
    let value = match raw {
        Some(s) => match s.trim().parse::<u32>() {
            Ok(v) => v,
            Err(_) => DEFAULT_MAX_FAILURES,
        },
        None => DEFAULT_MAX_FAILURES,
    };
    if value < MIN_MAX_FAILURES {
        tracing::warn!(
            configured = value,
            floor = MIN_MAX_FAILURES,
            "VICTAURI_MAX_FAILURES below minimum — clamping up to floor to avoid immediate recovery"
        );
        MIN_MAX_FAILURES
    } else {
        value
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("victauri-watchdog {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("victauri-watchdog {}", env!("CARGO_PKG_VERSION"));
        println!("Crash-recovery sidecar for Victauri MCP server\n");
        println!("USAGE: victauri-watchdog [PORT] [--app <identifier>]\n");
        println!("Without an explicit port, the port is discovered from the per-user");
        println!("discovery roots (<root>/<pid>/port, live apps only), falling back to 7373 when");
        println!("no --app is given. Once an app is found its identity is pinned: the");
        println!("watchdog follows only that app across restarts and never switches to a");
        println!("different one. With --app and no matching app, it reports the app DOWN.\n");
        println!("OPTIONS:");
        println!("  --app <id>       Watch the app with this bundle identifier / product name");
        println!("  -h, --help       Print help");
        println!("  -V, --version    Print version\n");
        println!("ENVIRONMENT:");
        println!("  VICTAURI_PORT           Server port (skips discovery)");
        println!("  VICTAURI_APP            Same as --app");
        println!("  VICTAURI_INTERVAL       Poll interval in seconds (default: 5, min: 1)");
        println!(
            "  VICTAURI_MAX_FAILURES   Consecutive failures before action (default: 3, min: 1)"
        );
        println!("  VICTAURI_ON_FAILURE     Shell command to run on failure");
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env();
    let mut selector = config.app.clone().map_or(Selector::Any, Selector::App);
    let discovered = if config.port_explicit {
        None
    } else {
        discover_app(&selector)
    };
    let mut target = initial_target(&config, &mut selector, discovered);
    match (&target, config.port_explicit) {
        (Some(t), false) if t.pid.is_some() => {
            tracing::info!(port = t.port, pid = ?t.pid, identity = ?selector, "Discovered Victauri app — identity pinned");
        }
        (Some(t), false) => tracing::info!(
            port = t.port,
            "No single live Victauri app discovered yet — polling the default port"
        ),
        (None, _) => tracing::warn!(
            app = ?config.app,
            "The selected Victauri app is not running (no matching discovery entry) — reporting it DOWN until it appears"
        ),
        _ => {}
    }

    tracing::info!(
        port = target.as_ref().map(|t| t.port),
        interval_secs = config.interval.as_secs(),
        max_failures = config.max_failures,
        // Program name only (R4-WD1): the full command line may carry secrets, tokens or
        // sensitive paths — see the same rule at the recovery site below.
        on_failure = on_failure_label(config.on_failure_cmd.as_deref()),
        "Victauri watchdog started"
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;

    let mut consecutive_failures: u32 = 0;
    let mut action_fired = false;

    loop {
        tokio::time::sleep(config.interval).await;

        match poll_health(&client, target.as_ref(), process::liveness).await {
            Ok(()) => {
                if consecutive_failures > 0 {
                    tracing::info!(
                        after_failures = consecutive_failures,
                        "Victauri MCP server recovered"
                    );
                    consecutive_failures = 0;
                    action_fired = false;
                }
            }
            Err(reason) => {
                consecutive_failures += 1;
                tracing::warn!(
                    failure_count = consecutive_failures,
                    reason,
                    "Victauri app down"
                );
            }
        }

        // The watched app may have restarted (new pid) on a different port (7373 taken →
        // 7374+). With no explicit port, re-resolve via discovery on failure — restricted to
        // the PINNED identity, so a different app is never silently adopted — and follow it;
        // the next successful poll then logs the recovery. Discovery only runs while
        // failing, so a healthy app costs no extra process spawns.
        if consecutive_failures > 0
            && !config.port_explicit
            && let rediscovered = discover_app(&selector)
            && let Some(next) = follow_target(target.as_ref(), &mut selector, rediscovered)
        {
            tracing::info!(
                from = ?target,
                to = ?next,
                identity = ?selector,
                "Watched Victauri app restarted — following it"
            );
            target = Some(next);
        }

        if consecutive_failures >= config.max_failures && !action_fired {
            tracing::error!(
                failure_count = consecutive_failures,
                "Victauri MCP server unreachable — the Tauri app may have crashed"
            );

            if let Some(ref cmd) = config.on_failure_cmd {
                // Do NOT log the full command line at info level — it may carry
                // secrets, tokens, or sensitive paths. Surface only the program
                // (first whitespace-delimited token). The full string is kept at
                // debug level for operators who explicitly opt in.
                tracing::info!(
                    program = recovery_program_name(cmd),
                    "Executing recovery action"
                );
                tracing::debug!(command = cmd, "Full recovery command line");
                match run_recovery(cmd).await {
                    Ok(status) => {
                        tracing::info!(exit_code = ?status.code(), "Recovery action completed");
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Recovery action failed to execute");
                    }
                }
            }

            action_fired = true;
        }
    }
}

/// Whether an HTTP status from `GET /health` proves the Victauri server is alive: a `2xx`,
/// or `429 Too Many Requests`. `/health` is unauthenticated and shares the public
/// rate-limit bucket, so any local process can flood it into 429s — and a rate-limited reply
/// is still the server answering. Counting 429 as a failure let that flood make the watchdog
/// run its recovery command against a healthy app (R4-NET1).
fn health_status_means_alive(status: u16) -> bool {
    (200..300).contains(&status) || status == 429
}

/// One poll: `Ok` when the watched app is up, else why it is not.
///
/// A watched pid that has exited is a crash even if some other app now answers on the same
/// port — never let a different app mask it. A pid whose liveness cannot be determined is
/// not treated as exited; `/health` decides.
async fn poll_health(
    client: &reqwest::Client,
    target: Option<&Target>,
    liveness: impl Fn(u32) -> process::Liveness,
) -> Result<(), String> {
    let Some(target) = target else {
        return Err(
            "the selected Victauri app is not running (no matching discovery entry)".to_string(),
        );
    };
    if target.pid.is_some_and(|pid| is_gone(liveness(pid))) {
        return Err("the watched app process has exited".to_string());
    }
    match client
        .get(format!("http://127.0.0.1:{}/health", target.port))
        .send()
        .await
    {
        Ok(resp) if health_status_means_alive(resp.status().as_u16()) => Ok(()),
        Ok(resp) => Err(format!(
            "health check returned non-success status {}",
            resp.status()
        )),
        Err(e) => Err(format!("health check failed: {e}")),
    }
}

/// What the startup log says about the recovery command: its program name only.
fn on_failure_label(cmd: Option<&str>) -> &str {
    cmd.map_or("(none)", recovery_program_name)
}

/// Extract the program name (first whitespace-delimited token) from a recovery
/// command line for non-sensitive logging. Returns `"(empty)"` for a
/// blank/whitespace-only command. This is a best-effort label for logs only —
/// the command is still executed verbatim through the shell.
fn recovery_program_name(cmd: &str) -> &str {
    cmd.split_whitespace().next().unwrap_or("(empty)")
}

async fn run_recovery(cmd: &str) -> anyhow::Result<std::process::ExitStatus> {
    // Spawn (not `.status()`) so we can enforce a timeout and kill a hung child —
    // a recovery command that never returns must not block the watchdog forever.
    let mut child = if cfg!(windows) {
        tokio::process::Command::new("cmd")
            .args(["/C", cmd])
            .spawn()?
    } else {
        tokio::process::Command::new("sh")
            .args(["-c", cmd])
            .spawn()?
    };
    match tokio::time::timeout(Duration::from_secs(RECOVERY_TIMEOUT_SECS), child.wait()).await {
        Ok(status) => Ok(status?),
        Err(_elapsed) => {
            // Kill the wrapping shell so the watchdog loop is freed. (A grandchild
            // the shell spawned may outlive it; the watchdog's job is to not wedge.)
            let _ = child.kill().await;
            anyhow::bail!(
                "recovery command timed out after {RECOVERY_TIMEOUT_SECS}s and was killed"
            );
        }
    }
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_env() {
        // SAFETY: test-only — ENV_LOCK serializes all env access in this module.
        unsafe {
            std::env::remove_var("VICTAURI_PORT");
            std::env::remove_var("VICTAURI_INTERVAL");
            std::env::remove_var("VICTAURI_MAX_FAILURES");
            std::env::remove_var("VICTAURI_ON_FAILURE");
            std::env::remove_var("VICTAURI_APP");
        }
    }

    #[test]
    fn parse_args_reads_port_and_app() {
        let a = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(parse_args(&a(&[])), (None, None));
        assert_eq!(parse_args(&a(&["7374"])), (Some(7374), None));
        assert_eq!(
            parse_args(&a(&["--app", "com.x.app", "7380"])),
            (Some(7380), Some("com.x.app".to_string()))
        );
        assert_eq!(
            parse_args(&a(&["--app=Demo"])),
            (None, Some("Demo".to_string()))
        );
    }

    fn write_entry(base: &Path, pid: u32, port: &str, identifier: Option<&str>) {
        let dir = base.join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(dir.join("port"), port).unwrap();
        if let Some(id) = identifier {
            let meta = serde_json::json!({"pid": pid, "identifier": id, "product_name": "P"});
            std::fs::write(dir.join("metadata.json"), meta.to_string()).unwrap();
        }
    }

    #[test]
    fn discovery_ignores_dead_pids_and_bad_ports() {
        let tmp = tempfile::tempdir().unwrap();
        write_entry(tmp.path(), 100, "7374", Some("com.a"));
        write_entry(tmp.path(), 200, "7373", Some("com.b")); // dead
        write_entry(tmp.path(), 300, "not-a-port", None);
        std::fs::create_dir_all(tmp.path().join("not-a-pid")).unwrap();
        let alive = |pid: u32| pid != 200;
        assert_eq!(
            discover_in(tmp.path(), &Selector::Any, alive),
            Discovery::Found(app(100, 7374, Some("com.a")))
        );
    }

    /// Audit N6: entries are read from every root (per-user + legacy); one pid counts once.
    #[test]
    fn discovery_merges_all_roots() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_entry(a.path(), 100, "7374", Some("com.a"));
        let roots = [a.path().to_path_buf(), b.path().to_path_buf()];
        let alive = |_| true;
        assert_eq!(
            discover_in_roots(&roots, &Selector::Any, alive),
            Discovery::Found(app(100, 7374, Some("com.a")))
        );
        write_entry(b.path(), 100, "7374", Some("com.a")); // same pid again: still one app
        assert_eq!(
            discover_in_roots(&roots, &Selector::Any, alive),
            Discovery::Found(app(100, 7374, Some("com.a")))
        );
        write_entry(b.path(), 200, "7375", Some("com.b"));
        assert_eq!(
            discover_in_roots(&roots, &Selector::Any, alive),
            Discovery::Ambiguous(vec![(100, 7374), (200, 7375)])
        );
        assert_eq!(
            discover_in_roots(&roots, &app_sel("com.b"), alive),
            Discovery::Found(app(200, 7375, Some("com.b")))
        );
    }

    #[cfg(unix)]
    #[test]
    fn discovery_roots_are_per_user_first() {
        let roots = discovery_roots();
        let euid = current_euid().unwrap();
        assert!(roots.contains(&std::env::temp_dir().join(format!("victauri-{euid}"))));
        assert_eq!(roots.last(), Some(&std::env::temp_dir().join("victauri")));
        assert_ne!(roots[0], std::env::temp_dir().join("victauri"));
        // R5B-LINDISC1: the home fallback is scanned after the per-user temp root, before
        // the legacy one.
        let home = home_state_root(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
            .expect("HOME is set in the test environment");
        let at = |p: &std::path::Path| roots.iter().position(|r| r == p);
        let tmp_root = std::env::temp_dir().join(format!("victauri-{euid}"));
        assert!(at(&home) > at(&tmp_root), "{roots:?}");
        assert!(at(&home) < Some(roots.len() - 1), "{roots:?}");
    }

    #[cfg(unix)]
    #[test]
    fn home_state_root_prefers_an_absolute_xdg_state_home() {
        assert_eq!(
            home_state_root(Some("/x/state".into()), Some("/home/u".into())),
            Some(std::path::PathBuf::from("/x/state/victauri"))
        );
        assert_eq!(
            home_state_root(Some("rel".into()), Some("/home/u".into())),
            Some(std::path::PathBuf::from("/home/u/.local/state/victauri"))
        );
        assert_eq!(home_state_root(None, Some("rel".into())), None);
    }

    fn app(pid: u32, port: u16, identity: Option<&str>) -> DiscoveredApp {
        DiscoveredApp {
            pid,
            port,
            identity: identity.map(str::to_string),
        }
    }

    fn app_sel(s: &str) -> Selector {
        Selector::App(s.to_string())
    }

    #[test]
    fn discovery_selects_by_app_identity() {
        let tmp = tempfile::tempdir().unwrap();
        write_entry(tmp.path(), 100, "7373", Some("com.a"));
        write_entry(tmp.path(), 200, "7375", Some("com.b"));
        let alive = |_| true;
        assert_eq!(
            discover_in(tmp.path(), &Selector::Any, alive),
            Discovery::Ambiguous(vec![(100, 7373), (200, 7375)])
        );
        assert_eq!(
            discover_in(tmp.path(), &app_sel("com.b"), alive),
            Discovery::Found(app(200, 7375, Some("com.b")))
        );
        // product_name also matches (both entries share "P" here → ambiguous).
        assert!(matches!(
            discover_in(tmp.path(), &app_sel("P"), alive),
            Discovery::Ambiguous(_)
        ));
        assert_eq!(
            discover_in(tmp.path(), &app_sel("com.missing"), alive),
            Discovery::None
        );
        assert_eq!(
            discover_in(&tmp.path().join("absent"), &Selector::Any, alive),
            Discovery::None
        );
    }

    #[test]
    fn pinned_identity_never_follows_a_different_app() {
        // The watched app (com.a, pid 100) died; the only live app is com.b. A pinned
        // watchdog must NOT adopt it — that would turn a crash into a silent green.
        let tmp = tempfile::tempdir().unwrap();
        write_entry(tmp.path(), 100, "7373", Some("com.a"));
        write_entry(tmp.path(), 200, "7374", Some("com.b"));
        let alive = |pid: u32| pid != 100;
        let pinned = Selector::Pinned("com.a".to_string());
        assert_eq!(discover_in(tmp.path(), &pinned, alive), Discovery::None);

        // …but it DOES follow the same app restarted under a new pid/port.
        write_entry(tmp.path(), 300, "7375", Some("com.a"));
        assert_eq!(
            discover_in(tmp.path(), &pinned, alive),
            Discovery::Found(app(300, 7375, Some("com.a")))
        );
    }

    #[test]
    fn pinned_identity_is_the_identifier_not_the_shared_product_name() {
        // Resolved by product_name "P" → pinned to the bundle identifier, so a different
        // app sharing the product name is not followed.
        let found = app(100, 7373, Some("com.a"));
        let pinned = app_sel("P").pin(&found);
        assert_eq!(pinned, Selector::Pinned("com.a".to_string()));
        let other = serde_json::json!({"identifier": "com.b", "product_name": "P"});
        assert!(!pinned.matches(200, &other));
        let same = serde_json::json!({"identifier": "com.a", "product_name": "P"});
        assert!(pinned.matches(300, &same));
        // An already-pinned selector stays pinned.
        assert_eq!(pinned.pin(&app(9, 1, Some("com.z"))), pinned);
    }

    /// R5-WD1: `--app` must match exactly like the CLI bridge (`matches_app`) and
    /// victauri-test discovery — ASCII case-insensitive, never a substring. A case-only
    /// mismatch used to be read as "app down" and fire the recovery command in a loop
    /// against a healthy app.
    #[test]
    fn app_selector_is_ascii_case_insensitive_like_the_bridge() {
        let meta = serde_json::json!({"identifier": "com.Mock.App", "product_name": "Mock App"});
        assert!(app_sel("com.mock.app").matches(1, &meta));
        assert!(app_sel("COM.MOCK.APP").matches(1, &meta));
        assert!(app_sel("mock app").matches(1, &meta));
        // Still exact: no substring / prefix matches.
        assert!(!app_sel("com.mock").matches(1, &meta));
        assert!(!app_sel("mock").matches(1, &meta));

        let tmp = tempfile::tempdir().unwrap();
        write_entry(tmp.path(), 100, "7373", Some("com.Mock.App"));
        assert_eq!(
            discover_in(tmp.path(), &app_sel("com.mock.app"), |_| true),
            Discovery::Found(app(100, 7373, Some("com.Mock.App")))
        );

        // The pinned identity follows the same rule (it is re-matched after a restart).
        let pinned = Selector::Pinned("com.Mock.App".to_string());
        let restarted = serde_json::json!({"identifier": "com.mock.app"});
        assert!(pinned.matches(2, &restarted));
        let other = serde_json::json!({"identifier": "com.mock.app.other"});
        assert!(!pinned.matches(3, &other));
    }

    #[test]
    fn identityless_entry_pins_to_its_pid() {
        let tmp = tempfile::tempdir().unwrap();
        write_entry(tmp.path(), 100, "7373", None);
        let Discovery::Found(found) = discover_in(tmp.path(), &Selector::Any, |_| true) else {
            panic!("expected one app");
        };
        assert_eq!(found.identity, None);
        let pinned = Selector::Any.pin(&found);
        assert_eq!(pinned, Selector::Pid(100));
        // A restart (new pid, no identity) can't be proven to be the same app → not followed.
        write_entry(tmp.path(), 200, "7374", None);
        let alive = |pid: u32| pid != 100;
        assert_eq!(discover_in(tmp.path(), &pinned, alive), Discovery::None);
    }

    fn test_config(app: Option<&str>, port_explicit: bool) -> Config {
        Config {
            port: if port_explicit { 9000 } else { DEFAULT_PORT },
            port_explicit,
            app: app.map(str::to_string),
            interval: Duration::from_secs(5),
            max_failures: 3,
            on_failure_cmd: None,
        }
    }

    #[test]
    fn initial_target_with_unmatched_app_reports_down_not_default_port() {
        let config = test_config(Some("com.missing"), false);
        let mut sel = app_sel("com.missing");
        assert_eq!(initial_target(&config, &mut sel, None), None);
        assert_eq!(sel, app_sel("com.missing"));
    }

    #[test]
    fn initial_target_pins_and_falls_back_correctly() {
        // Discovered → pinned + pid tracked.
        let config = test_config(None, false);
        let mut sel = Selector::Any;
        let t = initial_target(&config, &mut sel, Some(app(42, 7380, Some("com.a"))));
        assert_eq!(
            t,
            Some(Target {
                port: 7380,
                pid: Some(42)
            })
        );
        assert_eq!(sel, Selector::Pinned("com.a".to_string()));

        // No --app, nothing found → legacy default-port fallback.
        let mut sel = Selector::Any;
        assert_eq!(
            initial_target(&config, &mut sel, None),
            Some(Target {
                port: DEFAULT_PORT,
                pid: None
            })
        );

        // Explicit port wins, discovery ignored.
        let config = test_config(Some("com.a"), true);
        let mut sel = app_sel("com.a");
        assert_eq!(
            initial_target(&config, &mut sel, Some(app(1, 1, Some("com.a")))),
            Some(Target {
                port: 9000,
                pid: None
            })
        );
    }

    #[test]
    fn follow_target_only_reports_real_changes() {
        let mut sel = Selector::Pinned("com.a".to_string());
        let cur = Target {
            port: 7373,
            pid: Some(1),
        };
        assert_eq!(follow_target(Some(&cur), &mut sel, None), None);
        assert_eq!(
            follow_target(Some(&cur), &mut sel, Some(app(1, 7373, Some("com.a")))),
            None
        );
        assert_eq!(
            follow_target(Some(&cur), &mut sel, Some(app(2, 7374, Some("com.a")))),
            Some(Target {
                port: 7374,
                pid: Some(2)
            })
        );
        // From "down" (no target) the app appearing is a change; an App selector gets pinned.
        let mut sel = app_sel("P");
        assert!(follow_target(None, &mut sel, Some(app(5, 7373, Some("com.p")))).is_some());
        assert_eq!(sel, Selector::Pinned("com.p".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn untrusted_entries_and_base_are_ignored() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        // World-writable entry dir → ignored.
        write_entry(tmp.path(), 100, "7373", Some("com.a"));
        std::fs::set_permissions(
            tmp.path().join("100"),
            std::fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        // Symlinked entry dir → ignored.
        let real = tempfile::tempdir().unwrap();
        write_entry(real.path(), 200, "7374", Some("com.b"));
        std::os::unix::fs::symlink(real.path().join("200"), tmp.path().join("200")).unwrap();
        assert_eq!(
            discover_in(tmp.path(), &Selector::Any, |_| true),
            Discovery::None
        );

        // A group/other-writable BASE dir is untrusted as a whole.
        let base = tempfile::tempdir().unwrap();
        write_entry(base.path(), 300, "7375", Some("com.c"));
        std::fs::set_permissions(base.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(
            discover_in(base.path(), &Selector::Any, |_| true),
            Discovery::None
        );
        std::fs::set_permissions(base.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(
            discover_in(base.path(), &Selector::Any, |_| true),
            Discovery::Found(_)
        ));
    }

    /// R5B-WINDISC1: a directory owned by another account (SYSTEM stands in; setting that
    /// up needs an elevated run) is never followed.
    #[cfg(windows)]
    #[test]
    fn windows_entries_owned_by_another_account_are_ignored() {
        let give_to_system = |p: &Path| {
            std::process::Command::new("icacls")
                .arg(p)
                .args(["/setowner", "*S-1-5-18", "/q"])
                .output()
                .is_ok_and(|o| o.status.success())
        };
        let tmp = tempfile::tempdir().unwrap();
        write_entry(tmp.path(), 100, "7373", Some("com.a"));
        if !give_to_system(&tmp.path().join("100")) {
            eprintln!("skipped: cannot change a directory's owner (not elevated)");
            return;
        }
        assert_eq!(
            discover_in(tmp.path(), &Selector::Any, |_| true),
            Discovery::None
        );
        let base = tempfile::tempdir().unwrap();
        write_entry(base.path(), 300, "7375", Some("com.c"));
        assert!(matches!(
            discover_in(base.path(), &Selector::Any, |_| true),
            Discovery::Found(_)
        ));
        assert!(give_to_system(base.path()));
        assert_eq!(
            discover_in(base.path(), &Selector::Any, |_| true),
            Discovery::None
        );
    }

    #[cfg(unix)]
    #[test]
    fn trust_requires_ownership_by_current_user() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::tempdir().unwrap();
        let euid = current_euid().expect("euid probe");
        assert_eq!(std::fs::metadata(tmp.path()).unwrap().uid(), euid);
        assert!(dir_is_trusted(tmp.path()));
        // A directory owned by another user (root's `/`, when we are not root) is rejected
        // even though its mode (0755) has no group/other write bit.
        if euid != 0 {
            assert!(!dir_is_trusted(Path::new("/")));
        }
    }

    #[test]
    fn config_defaults() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_env();
        let config = Config::from_env();
        assert_eq!(config.port, 7373);
        assert!(!config.port_explicit);
        assert_eq!(config.interval, Duration::from_secs(5));
        assert_eq!(config.max_failures, 3);
        assert!(config.on_failure_cmd.is_none());
    }

    #[test]
    fn config_from_env_vars() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_env();
        // SAFETY: test-only — ENV_LOCK serializes all env access in this module.
        unsafe {
            std::env::set_var("VICTAURI_PORT", "9999");
            std::env::set_var("VICTAURI_INTERVAL", "10");
            std::env::set_var("VICTAURI_MAX_FAILURES", "5");
            std::env::set_var("VICTAURI_ON_FAILURE", "echo recovered");
        }
        let config = Config::from_env();
        assert_eq!(config.port, 9999);
        assert!(config.port_explicit, "VICTAURI_PORT must bypass discovery");
        assert_eq!(config.interval, Duration::from_secs(10));
        assert_eq!(config.max_failures, 5);
        assert_eq!(config.on_failure_cmd, Some("echo recovered".to_string()));
        clear_env();
    }

    #[test]
    fn config_invalid_env_uses_defaults() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_env();
        // SAFETY: test-only — ENV_LOCK serializes all env access in this module.
        unsafe {
            std::env::set_var("VICTAURI_PORT", "not_a_number");
            std::env::set_var("VICTAURI_INTERVAL", "abc");
            std::env::set_var("VICTAURI_MAX_FAILURES", "xyz");
        }
        let config = Config::from_env();
        assert_eq!(config.port, 7373);
        assert_eq!(config.interval, Duration::from_secs(5));
        assert_eq!(config.max_failures, 3);
        clear_env();
    }

    #[test]
    fn config_zero_interval_is_clamped() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_env();
        // SAFETY: test-only — ENV_LOCK serializes all env access in this module.
        unsafe {
            std::env::set_var("VICTAURI_INTERVAL", "0");
            std::env::set_var("VICTAURI_MAX_FAILURES", "0");
        }
        let config = Config::from_env();
        assert_eq!(config.interval, Duration::from_secs(MIN_INTERVAL_SECS));
        assert_eq!(config.max_failures, MIN_MAX_FAILURES);
        clear_env();
    }

    #[test]
    fn clamp_interval_floors_zero_and_sub_floor() {
        // Zero must clamp UP to the floor (never a busy loop).
        assert_eq!(
            clamp_interval(Some("0")),
            Duration::from_secs(MIN_INTERVAL_SECS)
        );
    }

    #[test]
    fn clamp_interval_preserves_valid_values() {
        assert_eq!(clamp_interval(Some("10")), Duration::from_secs(10));
        // Exactly at the floor is preserved.
        assert_eq!(
            clamp_interval(Some("1")),
            Duration::from_secs(MIN_INTERVAL_SECS)
        );
    }

    #[test]
    fn clamp_interval_defaults_when_unset_or_garbage() {
        assert_eq!(
            clamp_interval(None),
            Duration::from_secs(DEFAULT_INTERVAL_SECS)
        );
        assert_eq!(
            clamp_interval(Some("not_a_number")),
            Duration::from_secs(DEFAULT_INTERVAL_SECS)
        );
        // Whitespace is trimmed before parsing.
        assert_eq!(clamp_interval(Some("  7  ")), Duration::from_secs(7));
    }

    #[test]
    fn clamp_max_failures_floors_zero() {
        // Zero would mean "recover every poll" — clamp UP to 1.
        assert_eq!(clamp_max_failures(Some("0")), MIN_MAX_FAILURES);
    }

    #[test]
    fn clamp_max_failures_preserves_valid_values() {
        assert_eq!(clamp_max_failures(Some("5")), 5);
        assert_eq!(clamp_max_failures(Some("1")), MIN_MAX_FAILURES);
    }

    #[test]
    fn clamp_max_failures_defaults_when_unset_or_garbage() {
        assert_eq!(clamp_max_failures(None), DEFAULT_MAX_FAILURES);
        assert_eq!(clamp_max_failures(Some("xyz")), DEFAULT_MAX_FAILURES);
        assert_eq!(clamp_max_failures(Some("  4  ")), 4);
    }

    #[test]
    fn recovery_program_name_extracts_first_token() {
        assert_eq!(
            recovery_program_name("restart-app --token=SECRET --path /home/u"),
            "restart-app"
        );
        assert_eq!(recovery_program_name("echo"), "echo");
        assert_eq!(recovery_program_name("   "), "(empty)");
        assert_eq!(recovery_program_name(""), "(empty)");
    }

    /// A minimal HTTP server answering every request with `status`, on an ephemeral port.
    fn fixed_status_server(status: u16) -> u16 {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut sock in listener.incoming().flatten() {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf);
                let _ = write!(
                    sock,
                    "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
            }
        });
        port
    }

    fn target(port: u16, pid: Option<u32>) -> Target {
        Target { port, pid }
    }

    #[test]
    fn a_rate_limited_health_answer_means_alive() {
        assert!(health_status_means_alive(200));
        assert!(health_status_means_alive(429));
        assert!(!health_status_means_alive(401));
        assert!(!health_status_means_alive(503));
    }

    #[tokio::test]
    async fn a_health_flood_429_is_not_a_failure() {
        // R4-NET1: an unauthenticated /health flood (→ 429) used to count as a failure and,
        // after VICTAURI_MAX_FAILURES polls, ran the recovery command against a healthy app.
        let client = reqwest::Client::new();
        let own = |_| process::Liveness::Own;
        let limited = fixed_status_server(429);
        assert_eq!(
            poll_health(&client, Some(&target(limited, None)), own).await,
            Ok(())
        );
        let broken = fixed_status_server(503);
        let err = poll_health(&client, Some(&target(broken, None)), own)
            .await
            .unwrap_err();
        assert!(err.contains("503"), "{err}");
    }

    #[tokio::test]
    async fn an_undeterminable_pid_is_not_a_crash() {
        // R4-DISC1: with no working `kill`, every PID read dead and the watchdog fired
        // recovery on a healthy app. Unknown/unverified liveness must defer to /health.
        let client = reqwest::Client::new();
        let port = fixed_status_server(200);
        for liveness in [process::Liveness::Unknown, process::Liveness::Unverified] {
            assert_eq!(
                poll_health(&client, Some(&target(port, Some(42))), |_| liveness).await,
                Ok(()),
                "{liveness:?}"
            );
        }
        // A pid that is definitely gone (or recycled by another user) is still a crash, even
        // though something answers on the port.
        for liveness in [process::Liveness::Dead, process::Liveness::OtherUser] {
            let err = poll_health(&client, Some(&target(port, Some(42))), |_| liveness)
                .await
                .unwrap_err();
            assert!(err.contains("exited"), "{liveness:?}: {err}");
        }
    }

    #[test]
    fn discovery_follows_an_owner_whose_liveness_is_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        write_entry(tmp.path(), 100, "7374", Some("com.a"));
        assert!(may_be_alive(std::process::id()));
        assert_eq!(
            discover_in(tmp.path(), &Selector::Any, |_| !is_gone(
                process::Liveness::Unknown
            )),
            Discovery::Found(app(100, 7374, Some("com.a")))
        );
    }

    #[test]
    fn startup_log_names_only_the_recovery_program() {
        // R4-WD1: the startup INFO line logged the full recovery command line.
        let label = on_failure_label(Some("restart-app --token=SECRET --path /home/u"));
        assert_eq!(label, "restart-app");
        assert!(!label.contains("SECRET"));
        assert_eq!(on_failure_label(None), "(none)");
    }

    #[cfg(unix)]
    #[test]
    fn uid_probe_names_are_unpredictable() {
        let (a, b) = (unpredictable_suffix(), unpredictable_suffix());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn recovery_runs_echo() {
        let cmd = "echo ok";
        let status = run_recovery(cmd).await.unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    async fn recovery_bad_command_fails() {
        let cmd = if cfg!(windows) { "exit /b 1" } else { "exit 1" };
        let status = run_recovery(cmd).await.unwrap();
        assert!(!status.success());
    }
}
