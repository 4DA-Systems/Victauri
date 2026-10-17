//! `victauri setup <client>` / `victauri init --client <client>` — wire Victauri into an AI
//! agent client (MCP config + agent instructions) in one command.
//!
//! Every client gets the same server: `victauri bridge --app <identifier>`, the stdio proxy that
//! discovers the running app by identity. Config files are MERGED, never clobbered: JSON/JSONC is
//! edited by inserting one entry as text (see [`crate::jsonc`]), TOML through `toml_edit`, and
//! an existing `victauri` entry is always left exactly as the user has it. Project files are
//! written only inside the project (no symlinked components); user-level files are written only
//! for clients that have no project-level config, and only with `--user-config`.
//!
//! Formats (verified 2026-10 against each client's official docs):
//! - Claude Code — `.mcp.json`, `mcpServers` (<https://code.claude.com/docs/en/mcp>)
//! - Cursor — `.cursor/mcp.json`, `mcpServers`, `type: "stdio"` (<https://cursor.com/docs/context/mcp>)
//! - VS Code / Copilot — `.vscode/mcp.json`, `servers`, `type: "stdio"`, JSONC
//!   (<https://code.visualstudio.com/docs/copilot/customization/mcp-servers>)
//! - Windsurf (now Devin Desktop) — user-level `mcp_config.json` only
//!   (<https://docs.devin.ai/desktop/cascade/mcp>)
//! - Codex — `.codex/config.toml` (trusted projects) or `~/.codex/config.toml`,
//!   `[mcp_servers.<name>]` (<https://learn.chatgpt.com/docs/extend/mcp?surface=cli>)
//! - Gemini CLI — `.gemini/settings.json`, `mcpServers` (<https://geminicli.com/docs/tools/mcp-server/>)
//! - Zed — `.zed/settings.json`, `context_servers` (<https://zed.dev/docs/ai/mcp>)
//! - Roo Code — `.roo/mcp.json`, `mcpServers`
//!   (<https://roocodeinc.github.io/Roo-Code/features/mcp/using-mcp-in-roo>)

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::jsonc::{self, JsonMerge};
use crate::{display_path, ensure_inside_project, refuse_symlink};

/// A client named on the command line (`all` expands to every client).
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ClientArg {
    /// Claude Code — `.mcp.json` + CLAUDE.md block + `.claude/skills/victauri/SKILL.md`
    Claude,
    /// Cursor — `.cursor/mcp.json` + AGENTS.md block
    Cursor,
    /// VS Code / GitHub Copilot — `.vscode/mcp.json` + AGENTS.md block
    #[value(alias = "copilot")]
    Vscode,
    /// Windsurf / Devin Desktop — user-level `mcp_config.json` (needs --user-config) + AGENTS.md
    #[value(alias = "devin")]
    Windsurf,
    /// `OpenAI` Codex — `.codex/config.toml` (or `~/.codex` with --user-config) + AGENTS.md
    Codex,
    /// Gemini CLI — `.gemini/settings.json` + AGENTS.md + a GEMINI.md import
    Gemini,
    /// Zed — `.zed/settings.json` (`context_servers`) + AGENTS.md
    Zed,
    /// Roo Code — `.roo/mcp.json` + AGENTS.md
    Roo,
    /// Every client above
    All,
}

/// A concrete client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Client {
    Claude,
    Cursor,
    Vscode,
    Windsurf,
    Codex,
    Gemini,
    Zed,
    Roo,
}

impl Client {
    const ALL: [Self; 8] = [
        Self::Claude,
        Self::Cursor,
        Self::Vscode,
        Self::Windsurf,
        Self::Codex,
        Self::Gemini,
        Self::Zed,
        Self::Roo,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Cursor => "Cursor",
            Self::Vscode => "VS Code / Copilot",
            Self::Windsurf => "Windsurf / Devin Desktop",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini CLI",
            Self::Zed => "Zed",
            Self::Roo => "Roo Code",
        }
    }
}

/// Expand the command-line selection into concrete clients, de-duplicated, in a stable order.
#[must_use]
pub fn expand(args: &[ClientArg]) -> Vec<Client> {
    let wanted = |c: Client| {
        args.iter().any(|a| {
            *a == ClientArg::All
                || matches!(
                    (a, c),
                    (ClientArg::Claude, Client::Claude)
                        | (ClientArg::Cursor, Client::Cursor)
                        | (ClientArg::Vscode, Client::Vscode)
                        | (ClientArg::Windsurf, Client::Windsurf)
                        | (ClientArg::Codex, Client::Codex)
                        | (ClientArg::Gemini, Client::Gemini)
                        | (ClientArg::Zed, Client::Zed)
                        | (ClientArg::Roo, Client::Roo)
                )
        })
    };
    Client::ALL.into_iter().filter(|c| wanted(*c)).collect()
}

/// What `setup` should do.
#[derive(Debug, Clone)]
pub struct Options {
    pub clients: Vec<Client>,
    /// Allow writing user-level config files (Windsurf/Devin; Codex's `~/.codex/config.toml`).
    pub user_config: bool,
    /// Also write agent instructions (CLAUDE.md / AGENTS.md / skill). On by default.
    pub instructions: bool,
    /// Report what would change without writing anything.
    pub dry_run: bool,
    /// The app gates Victauri behind its `victauri` Cargo feature (`init --optional`): the
    /// instructions then say to start it with `--features victauri`.
    pub feature_gated: bool,
}

/// The user's home-ish directories, injected so tests never touch the real ones.
#[derive(Debug, Clone, Default)]
pub struct UserDirs {
    pub home: Option<PathBuf>,
    /// `%APPDATA%` (Windows).
    pub appdata: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME`.
    pub xdg_config: Option<PathBuf>,
    /// `$CODEX_HOME` (Codex's config dir override).
    pub codex_home: Option<PathBuf>,
}

impl UserDirs {
    /// Read the directories from the environment.
    #[must_use]
    pub fn from_env() -> Self {
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Self {
            // Windows tools (Codex included) resolve `~` to %USERPROFILE%; a Git-Bash/MSYS
            // `HOME` can point elsewhere, so it is only the fallback there.
            home: if cfg!(windows) {
                var("USERPROFILE").or_else(|| var("HOME"))
            } else {
                var("HOME").or_else(|| var("USERPROFILE"))
            },
            appdata: var("APPDATA"),
            xdg_config: var("XDG_CONFIG_HOME"),
            codex_home: var("CODEX_HOME"),
        }
    }

    /// Windsurf / Devin Desktop's user-level `mcp_config.json` (current docs), plus the legacy
    /// Windsurf path when that file already exists (older installs still read it).
    fn windsurf_configs(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let current = if cfg!(windows) {
            self.appdata
                .as_ref()
                .map(|a| a.join("devin").join("mcp_config.json"))
        } else {
            self.xdg_config
                .clone()
                .or_else(|| self.home.as_ref().map(|h| h.join(".config")))
                .map(|c| c.join("devin").join("mcp_config.json"))
        };
        out.extend(current);
        if let Some(home) = &self.home {
            let legacy = home
                .join(".codeium")
                .join("windsurf")
                .join("mcp_config.json");
            if legacy.is_file() {
                out.push(legacy);
            }
        }
        out
    }

    fn codex_config(&self) -> Option<PathBuf> {
        self.codex_home
            .clone()
            .or_else(|| self.home.as_ref().map(|h| h.join(".codex")))
            .map(|d| d.join("config.toml"))
    }
}

/// How a config file stores MCP servers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    /// JSON/JSONC: servers live in the object at top-level `key`; `typed` adds `"type": "stdio"`.
    Json { key: &'static str, typed: bool },
    /// Codex `config.toml`: `[mcp_servers.<name>]`.
    CodexToml,
}

/// One config file a client reads.
#[derive(Clone, Debug)]
struct ConfigTarget {
    path: PathBuf,
    format: Format,
    user_level: bool,
}

/// What happened to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Created,
    Updated,
    Unchanged(String),
    Skipped(String),
}

/// One line of the setup report.
#[derive(Debug, Clone)]
pub struct Report {
    pub path: PathBuf,
    pub what: String,
    pub status: Status,
    /// For a skipped config: the entry to add by hand.
    pub manual: Option<String>,
    pub user_level: bool,
}

/// The bridge arguments every client runs.
fn bridge_args(app: Option<&str>) -> Vec<&str> {
    let mut args = vec!["bridge"];
    if let Some(id) = app {
        args.extend(["--app", id]);
    }
    args
}

/// The server entry for a JSON client.
fn json_entry(app: Option<&str>, typed: bool) -> Value {
    let mut entry = serde_json::json!({ "command": "victauri", "args": bridge_args(app) });
    if typed && let Some(obj) = entry.as_object_mut() {
        obj.insert("type".into(), Value::from("stdio"));
    }
    entry
}

/// The config files `client` should get. `Err` carries the reason it gets none.
fn config_targets(
    client: Client,
    root: &Path,
    dirs: &UserDirs,
    user_config: bool,
) -> std::result::Result<Vec<ConfigTarget>, String> {
    let project = |rel: &str, format: Format| {
        Ok(vec![ConfigTarget {
            path: rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c)),
            format,
            user_level: false,
        }])
    };
    let mcp_servers = Format::Json {
        key: "mcpServers",
        typed: false,
    };
    match client {
        Client::Claude => project(".mcp.json", mcp_servers),
        Client::Cursor => project(
            ".cursor/mcp.json",
            Format::Json {
                key: "mcpServers",
                typed: true,
            },
        ),
        Client::Vscode => project(
            ".vscode/mcp.json",
            Format::Json {
                key: "servers",
                typed: true,
            },
        ),
        Client::Gemini => project(".gemini/settings.json", mcp_servers),
        Client::Zed => project(
            ".zed/settings.json",
            Format::Json {
                key: "context_servers",
                typed: false,
            },
        ),
        Client::Roo => project(".roo/mcp.json", mcp_servers),
        Client::Codex if user_config => dirs
            .codex_config()
            .map(|path| {
                vec![ConfigTarget {
                    path,
                    format: Format::CodexToml,
                    user_level: true,
                }]
            })
            .ok_or_else(|| "no home directory found (set HOME or CODEX_HOME)".to_string()),
        Client::Codex => project(".codex/config.toml", Format::CodexToml),
        Client::Windsurf if user_config => {
            let paths = dirs.windsurf_configs();
            if paths.is_empty() {
                return Err("no APPDATA / home directory found".to_string());
            }
            Ok(paths
                .into_iter()
                .map(|path| ConfigTarget {
                    path,
                    format: mcp_servers,
                    user_level: true,
                })
                .collect())
        }
        Client::Windsurf => Err("this client has no project-level MCP config — re-run with \
             --user-config to add Victauri to your user-level mcp_config.json"
            .to_string()),
    }
}

/// Merge the Victauri server into a Codex `config.toml` (`None` = new file).
fn merge_codex_toml(existing: Option<&str>, app: Option<&str>) -> JsonMerge {
    let text = existing.unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = match text.parse() {
        Ok(d) => d,
        Err(e) => {
            return JsonMerge::Refused(format!(
                "it is not valid TOML ({}) — left untouched",
                victauri_test::terminal::single_line(&e.to_string())
            ));
        }
    };
    let servers = doc
        .entry("mcp_servers")
        .or_insert_with(|| {
            let mut t = toml_edit::Table::new();
            t.set_implicit(true);
            toml_edit::Item::Table(t)
        })
        .as_table_like_mut();
    let Some(servers) = servers else {
        return JsonMerge::Refused("its mcp_servers key is not a table — left untouched".into());
    };
    if servers.contains_key("victauri") {
        return JsonMerge::AlreadyPresent;
    }
    let mut entry = toml_edit::Table::new();
    entry.insert("command", toml_edit::value("victauri"));
    let mut args = toml_edit::Array::new();
    for a in bridge_args(app) {
        args.push(a);
    }
    entry.insert("args", toml_edit::value(args));
    servers.insert("victauri", toml_edit::Item::Table(entry));
    let out = doc.to_string();
    // Verify: the result parses, carries the entry, and kept every original top-level key.
    let ok = out.parse::<toml_edit::DocumentMut>().is_ok_and(|d| {
        d.get("mcp_servers")
            .and_then(|s| s.get("victauri"))
            .and_then(|v| v.get("command"))
            .and_then(toml_edit::Item::as_str)
            == Some("victauri")
            && text
                .parse::<toml_edit::DocumentMut>()
                .is_ok_and(|o| o.iter().all(|(k, _)| d.contains_key(k)))
    });
    if ok {
        JsonMerge::Write(out)
    } else {
        JsonMerge::Refused("the merge could not be verified — left untouched".into())
    }
}

/// The text to add by hand when a config is skipped.
fn manual_entry(format: Format, app: Option<&str>) -> String {
    match format {
        Format::Json { key, typed } => {
            let entry = jsonc::render_inline(&json_entry(app, typed));
            format!("in \"{key}\": \"victauri\": {entry}")
        }
        Format::CodexToml => {
            let args: Vec<String> = bridge_args(app)
                .iter()
                .map(|a| Value::from(*a).to_string())
                .collect();
            format!(
                "[mcp_servers.victauri] command = \"victauri\" args = [{}]",
                args.join(", ")
            )
        }
    }
}

/// Read `path` as UTF-8 if it exists. A symlink is refused (never read or written through).
fn read_existing(path: &Path) -> Result<Option<String>> {
    refuse_symlink(path)?;
    match std::fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() => {
            anyhow::bail!("{} is not a regular file", display_path(path))
        }
        Ok(_) => std::fs::read_to_string(path)
            .map(Some)
            .with_context(|| format!("failed to read {}", display_path(path))),
        Err(_) => Ok(None),
    }
}

/// Write `contents` to `path`, replacing it atomically (temp file + rename in the same
/// directory) so an interrupted write can never leave a half-written config behind.
fn write_atomically(path: &Path, contents: &str) -> Result<()> {
    refuse_symlink(path)?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)
        .with_context(|| format!("failed to create {}", display_path(dir)))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.victauri-{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .with_context(|| format!("failed to create {}", display_path(&tmp)))?;
        std::io::Write::write_all(&mut file, contents.as_bytes())
            .with_context(|| format!("failed to write {}", display_path(&tmp)))?;
        file.sync_all().ok();
        drop(file);
        std::fs::rename(&tmp, path)
            .with_context(|| format!("failed to replace {}", display_path(path)))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Merge the server into one config file.
fn apply_config(
    root: &Path,
    target: &ConfigTarget,
    app: Option<&str>,
    dry_run: bool,
) -> Result<Report> {
    if !target.user_level {
        ensure_inside_project(root, &target.path)?;
    }
    let existing = read_existing(&target.path)?;
    let merge = match target.format {
        Format::Json { key, typed } => jsonc::insert_server(
            existing.as_deref(),
            key,
            "victauri",
            &json_entry(app, typed),
        ),
        Format::CodexToml => merge_codex_toml(existing.as_deref(), app),
    };
    let mut report = Report {
        path: target.path.clone(),
        what: "MCP server".into(),
        status: Status::Created,
        manual: None,
        user_level: target.user_level,
    };
    match merge {
        JsonMerge::Write(text) => {
            report.status = if existing.is_some() {
                Status::Updated
            } else {
                Status::Created
            };
            if !dry_run {
                write_atomically(&target.path, &text)?;
            }
        }
        JsonMerge::AlreadyPresent => {
            report.status =
                Status::Unchanged("already has a \"victauri\" server (kept as is)".into());
        }
        JsonMerge::Refused(why) => {
            report.status = Status::Skipped(why);
            report.manual = Some(manual_entry(target.format, app));
        }
    }
    Ok(report)
}

/// The marker every generated instruction block starts with (idempotency + opt-out).
pub const BLOCK_BEGIN: &str = "VICTAURI:BEGIN";

/// Add `block` to the Markdown file at `path` unless a Victauri block is already there.
fn apply_marker_block(
    root: &Path,
    path: &Path,
    block: &str,
    what: &str,
    dry_run: bool,
) -> Result<Report> {
    ensure_inside_project(root, path)?;
    let existing = read_existing(path)?;
    let status = match &existing {
        Some(content) if content.contains(BLOCK_BEGIN) => {
            Status::Unchanged("already has the Victauri block (VICTAURI markers)".into())
        }
        Some(content) => {
            if !dry_run {
                let mut joined = content.clone();
                if !joined.is_empty() && !joined.ends_with('\n') {
                    joined.push('\n');
                }
                joined.push_str(block);
                write_atomically(path, &joined)?;
            }
            Status::Updated
        }
        None => {
            if !dry_run {
                write_atomically(path, block.trim_start_matches('\n'))?;
            }
            Status::Created
        }
    };
    Ok(Report {
        path: path.to_path_buf(),
        what: what.into(),
        status,
        manual: None,
        user_level: false,
    })
}

/// Create a whole file unless something is already there (the user may have edited it).
fn apply_new_file(
    root: &Path,
    path: &Path,
    contents: &str,
    what: &str,
    dry_run: bool,
) -> Result<Report> {
    ensure_inside_project(root, path)?;
    let status = if read_existing(path)?.is_some() {
        Status::Unchanged("already exists (kept as is — delete it to regenerate)".into())
    } else {
        if !dry_run {
            write_atomically(path, contents)?;
        }
        Status::Created
    };
    Ok(Report {
        path: path.to_path_buf(),
        what: what.into(),
        status,
        manual: None,
        user_level: false,
    })
}

/// Wire the selected clients into the project at `root` (canonical). Never fails on one
/// client's malformed config — that file is skipped with the entry to add by hand — but does
/// fail on an unsafe path (symlink / outside the project) or an I/O error.
///
/// # Errors
/// An unsafe write target or an I/O error.
pub fn setup(
    root: &Path,
    app: Option<&str>,
    opts: &Options,
    dirs: &UserDirs,
) -> Result<Vec<Report>> {
    let mut reports = Vec::new();
    for client in &opts.clients {
        match config_targets(*client, root, dirs, opts.user_config) {
            Ok(targets) => {
                for t in targets {
                    let mut r = apply_config(root, &t, app, opts.dry_run)?;
                    r.what = format!("{} MCP server", client.label());
                    reports.push(r);
                }
            }
            Err(why) => reports.push(Report {
                path: PathBuf::new(),
                what: format!("{} MCP server", client.label()),
                status: Status::Skipped(why),
                manual: Some(manual_entry(
                    Format::Json {
                        key: "mcpServers",
                        typed: false,
                    },
                    app,
                )),
                user_level: true,
            }),
        }
    }
    if !opts.instructions {
        return Ok(reports);
    }
    let has = |c: Client| opts.clients.contains(&c);
    if has(Client::Claude) {
        reports.push(apply_marker_block(
            root,
            &root.join("CLAUDE.md"),
            &claude_md_block(opts.feature_gated),
            "Claude Code instructions",
            opts.dry_run,
        )?);
        let skill = root
            .join(".claude")
            .join("skills")
            .join("victauri")
            .join("SKILL.md");
        reports.push(apply_new_file(
            root,
            &skill,
            &skill_md(opts.feature_gated),
            "Claude Code skill",
            opts.dry_run,
        )?);
    }
    if opts.clients.iter().any(|c| *c != Client::Claude) {
        reports.push(apply_marker_block(
            root,
            &root.join("AGENTS.md"),
            &agents_md_block(opts.feature_gated),
            "AGENTS.md instructions",
            opts.dry_run,
        )?);
    }
    if has(Client::Gemini) {
        reports.push(apply_marker_block(
            root,
            &root.join("GEMINI.md"),
            GEMINI_MD_BLOCK,
            "Gemini CLI context (imports AGENTS.md)",
            opts.dry_run,
        )?);
    }
    Ok(reports)
}

/// Print a setup report the way the rest of `victauri init` reports.
pub fn print_reports(root: &Path, reports: &[Report], dry_run: bool) {
    let shown = |p: &Path| {
        p.strip_prefix(root).map_or_else(
            |_| display_path(p),
            |rel| victauri_test::terminal::single_line(&rel.display().to_string()),
        )
    };
    let would = if dry_run { "would be " } else { "" };
    for r in reports {
        let path = shown(&r.path);
        let scope = if r.user_level && !r.path.as_os_str().is_empty() {
            " [user-level]"
        } else {
            ""
        };
        match &r.status {
            Status::Created => eprintln!("  [+] {path}{scope} {would}created — {}", r.what),
            Status::Updated => {
                eprintln!("  [+] {path}{scope} {would}updated — added {}", r.what);
            }
            Status::Unchanged(why) => eprintln!("  [=] {path}{scope} {why} — {}", r.what),
            Status::Skipped(why) => {
                let target = if path.is_empty() {
                    String::new()
                } else {
                    format!("{path}: ")
                };
                eprintln!("  [!] {} skipped — {target}{why}", r.what);
                if let Some(manual) = &r.manual {
                    eprintln!(
                        "      Add by hand: {}",
                        victauri_test::terminal::single_line(manual)
                    );
                }
            }
        }
    }
}

/// Which clients already have a `victauri` server in this project (for `victauri doctor`).
#[must_use]
pub fn configured_project_clients(root: &Path) -> Vec<(&'static str, PathBuf)> {
    let dirs = UserDirs::default();
    Client::ALL
        .into_iter()
        .filter_map(|c| {
            let targets = config_targets(c, root, &dirs, false).ok()?;
            let t = targets.into_iter().next()?;
            let text = std::fs::read_to_string(&t.path).ok()?;
            let present = match t.format {
                Format::Json { key, .. } => {
                    serde_json::from_str::<Value>(&jsonc::blank_jsonc(&text))
                        .ok()
                        .is_some_and(|v| v.get(key).and_then(|s| s.get("victauri")).is_some())
                }
                Format::CodexToml => text
                    .parse::<toml_edit::DocumentMut>()
                    .ok()
                    .is_some_and(|d| {
                        d.get("mcp_servers")
                            .and_then(|s| s.get("victauri"))
                            .is_some()
                    }),
            };
            present.then_some((c.label(), t.path))
        })
        .collect()
}

// ── Agent instructions ──────────────────────────────────────────────────────────────────

const BLOCK_HEADER: &str =
    "<!-- VICTAURI:BEGIN (added by `victauri init` — delete this block to opt out) -->";
const BLOCK_FOOTER: &str = "<!-- VICTAURI:END -->";

/// The one source of the agent guidance. `{{MCP_CONFIG}}` (sentence start) and
/// `{{MCP_CONFIG_REF}}` (mid-sentence) name the MCP config file(s).
const GUIDANCE_TEMPLATE: &str = r#"## Victauri (App Inspection & Testing)

This app has **Victauri** integrated — an MCP server embedded inside the Tauri process
that gives full-stack access to the webview DOM, IPC layer, Rust backend, and native
windows. Available when the app is running in debug mode.

**Use Victauri MCP tools for all app inspection and testing tasks.** Victauri runs inside
the app process with direct AppHandle access (Rust-side tools answer in well under a
millisecond; webview tools are a JS round trip) — it sees everything, not just the webview.

Key Victauri tools (the read-only backend/DB introspection below has no CDP/Playwright
equivalent — and on macOS/Linux CDP can't attach to a Tauri webview at all):
- `invoke_command` — call any registered Tauri command directly (also records timing and
  honours fault injection; an eval-capable tool can also reach `__TAURI_INTERNALS__.invoke`)
- `verify_state` — cross-boundary frontend/backend state verification
- `detect_ghost_commands` — find frontend calls absent from the introspection registry (read the `reliability` field: only a real "no backend handler" bug when the registry mirrors the app's full command set; with no/partial `#[inspectable]` it lists real, uninstrumented commands)
- `check_ipc_integrity` — verify IPC pipeline health
- `introspect` — command timings, IPC command catalog (real arg/result shapes), contract
  testing, coverage, startup timing, capabilities, DB health, event bus
- `fault` — inject faults (delay, error, drop, corrupt) into commands **driven through
  Victauri's own `invoke_command`** — it tests a handler's error path when *you* call it; it
  does NOT intercept the app's real frontend IPC (a user clicking the UI is unaffected)
- `explain` — natural-language narration of what happened in the app
- `get_memory_stats` — real OS process memory stats
- `inspect` — `audit_accessibility` (WCAG checks), `get_performance` (navigation timing, JS
  heap, resource loading), `get_styles`, `get_bounding_boxes`, `highlight`

### Connecting reliably (read this before reaching for CDP)

{{MCP_CONFIG}} connects through `victauri bridge` — a stdio proxy that **discovers the running
app's port at connect time and re-discovers on restart**, so you are never pinned to a stale
or wrong port. Do **not** replace it with a fixed `"url": "http://127.0.0.1:7373/mcp"`: a
hardcoded port can point at a *different* Victauri app (e.g. a leftover demo) and every call
then fails with `422`/`404`. The bridge avoids that by design.

- **The server connects even before the app is running.** The bridge answers the MCP
  handshake itself, so `victauri` shows up connected in a fresh terminal whether or not the
  app is up. While the app is down the tool list is a static fallback and a tool *call*
  returns a clear "backend not reachable — start the app" error (not a hang). Start the app
  (`npm run tauri dev` / `pnpm tauri dev`) and the bridge connects automatically — tools go
  live with **no `/mcp` reconnect**. So if a call reports the backend unreachable, the fix is
  to start/await the app, not to switch to CDP.
- **First contact:** call `get_plugin_info` once and check `app.identifier` — confirm you
  reached the intended app, not another Victauri instance.
- **Multiple apps running?** Pin the bridge with `--app <bundle-identifier>` in {{MCP_CONFIG_REF}}
  (`"args": ["bridge", "--app", "com.your.app"]`), or set `VICTAURI_APP`. `init` bakes this in
  automatically when it can read your identifier.
- **If a tool call fails after an app rebuild/restart:** the bridge re-resolves the backend
  automatically — just retry (the transport is stateless by default, so there is usually no
  session to lose). If the MCP path is genuinely wedged, the **sessionless
  REST API is the fallback, NOT CDP**: `POST http://127.0.0.1:<port>/api/tools/<tool>` with
  the app's Bearer token (same capabilities, no session). Run `victauri check` (add
  `--app <bundle-identifier>` if several apps run) to see the endpoint; the token is the
  `token` file in the app's `<pid>` directory under the **per-user** discovery root —
  `$XDG_RUNTIME_DIR/victauri/` (else `/tmp/victauri-<uid>/`) on Linux/macOS,
  `%TEMP%\victauri\` on Windows. Never take a token from a shared, world-writable directory
  such as a bare `/tmp/victauri/`: another local user can plant entries there.

### Awaiting async backend work (don't guess with sleeps)

Many Tauri commands are **fire-and-forget**: they spawn background work and return
immediately (often `null`) while the real work runs. Don't poll by hand or sprinkle fixed
`sleep`s — use `wait_for` to await true completion:

- **Pollable status (no app changes):** `wait_for` with `condition: "expression"` evaluates a
  JS expression every poll until it is truthy (or equals `expected`). It may `await`, so you
  can await a status command directly:
  `wait_for { condition: "expression", value: "(await window.__TAURI_INTERNALS__.invoke('get_status')).running === false" }`.
  Level-triggered and race-free.
- **Completion event:** `wait_for` with `condition: "event"` blocks until a named Tauri event
  fires, with a `since_ms` look-back so an event emitted in the gap after your `invoke_command`
  is still caught: `wait_for { condition: "event", value: "analysis-complete" }`. (Custom events
  must be registered via `VictauriBuilder::listen_events(&["…"])`.)

The robust pattern is `invoke_command(...)` then `wait_for(expression|event, ...)` — never a bare sleep.

### Reading app-specific backend state

If the app registers state probes (`VictauriBuilder::probe("name", || json!({...}))`), call
`app_state` to read domain state directly from the Rust process — no IPC round-trip, no log
grepping. `app_state` with no args lists probe names; `app_state { probe: "name" }` returns its
snapshot. Use this for pipeline/queue/cache internals (version, depth, stats) instead of
reverse-engineering them from `query_db` + logs.

### Driving specific code paths & mutating test state

- To exercise a specific backend code path, call the relevant command via `invoke_command`
  (with args). If the path you need isn't reachable from any command, that's an app gap — add a
  small debug command and drive it.
- `query_db` is intentionally **read-only**. To mutate state for a test, go through the app's
  own commands with `invoke_command` (which respects app invariants) rather than writing the DB.

Prefer Victauri over Playwright or CDP for any Tauri-app task it handles; fall back to
Playwright only for browser-only work unrelated to this app.
"#;

/// The agent guidance with the MCP-config placeholder filled in.
#[must_use]
pub fn guidance(sentence_start: &str, mid_sentence: &str) -> String {
    GUIDANCE_TEMPLATE
        .replace("{{MCP_CONFIG_REF}}", mid_sentence)
        .replace("{{MCP_CONFIG}}", sentence_start)
}

/// For an app that gates Victauri behind its `victauri` Cargo feature (`init --optional`): a
/// plain `tauri dev` builds without Victauri, and the bridge then never finds the app.
const FEATURE_GATED_NOTE: &str = "\n**This app gates Victauri behind the `victauri` Cargo \
feature.** Start it with\n`npm run tauri dev -- --features victauri` / `pnpm tauri dev --features \
victauri`; a\nplain `tauri dev` builds without Victauri and every tool call reports the backend\n\
unreachable. Release builds leave the feature off and contain no Victauri code.\n";

/// The guidance body, plus the feature note for a feature-gated app.
fn body(sentence_start: &str, mid_sentence: &str, feature_gated: bool) -> String {
    let mut body = guidance(sentence_start, mid_sentence);
    if feature_gated {
        body.push_str(FEATURE_GATED_NOTE);
    }
    body
}

fn marker_block(body: &str) -> String {
    format!("\n{BLOCK_HEADER}\n{body}{BLOCK_FOOTER}\n")
}

/// The CLAUDE.md block (unchanged from earlier releases unless `feature_gated`: Claude Code
/// reads `.mcp.json`).
#[must_use]
pub fn claude_md_block(feature_gated: bool) -> String {
    marker_block(&body("`.mcp.json`", "`.mcp.json`", feature_gated))
}

/// The AGENTS.md block — the cross-agent convention read by Codex, Cursor, VS Code / Copilot,
/// Gemini CLI (via GEMINI.md import), Zed, Windsurf/Devin, Roo and others (<https://agents.md>).
#[must_use]
pub fn agents_md_block(feature_gated: bool) -> String {
    marker_block(&body(
        "Your client's MCP config (`.mcp.json`, `.cursor/mcp.json`, `.vscode/mcp.json`, \
         `.codex/config.toml`, `.gemini/settings.json`, `.zed/settings.json`, `.roo/mcp.json`, \
         or Windsurf/Devin's user-level `mcp_config.json` — `victauri setup <client>` writes \
         it)",
        "your client's MCP config",
        feature_gated,
    ))
}

/// Gemini CLI loads `GEMINI.md` by default; this block imports AGENTS.md so the guidance has
/// one source (<https://geminicli.com/docs/cli/gemini-md/>, `@file` imports).
const GEMINI_MD_BLOCK: &str = "\n<!-- VICTAURI:BEGIN (added by `victauri init` — delete this block to opt out) -->\n\
Victauri agent guidance for this Tauri app lives in AGENTS.md:\n\n@./AGENTS.md\n<!-- VICTAURI:END -->\n";

/// The project skill (`.claude/skills/victauri/SKILL.md`). Frontmatter follows the portable
/// Agent Skills spec (<https://agentskills.io/specification>): `name` matches the directory,
/// `description` ≤ 1024 characters.
#[must_use]
pub fn skill_md(feature_gated: bool) -> String {
    format!(
        "---\nname: victauri\ndescription: {SKILL_DESCRIPTION}\n---\n\n\
         <!-- Generated by `victauri setup claude`. Delete this directory to opt out. -->\n\n{}",
        body("`.mcp.json`", "`.mcp.json`", feature_gated)
    )
}

const SKILL_DESCRIPTION: &str = "Inspect, test and debug this Tauri app through the Victauri MCP \
server embedded in the running debug build - webview DOM, IPC calls with request and response \
bodies, Rust backend state probes, the app database (read-only), native windows and screenshots. \
Use whenever a task involves running, inspecting, testing, verifying or debugging this Tauri app; \
prefer it over CDP, Playwright or browser automation.";

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(clients: &[Client]) -> Options {
        Options {
            clients: clients.to_vec(),
            user_config: false,
            instructions: true,
            dry_run: false,
            feature_gated: false,
        }
    }

    fn root() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        (dir, root)
    }

    fn json(path: &Path) -> Value {
        serde_json::from_str(&jsonc::blank_jsonc(&std::fs::read_to_string(path).unwrap())).unwrap()
    }

    #[test]
    fn expand_all_and_dedup() {
        assert_eq!(expand(&[ClientArg::All]), Client::ALL.to_vec());
        assert_eq!(
            expand(&[ClientArg::Vscode, ClientArg::Claude, ClientArg::Vscode]),
            vec![Client::Claude, Client::Vscode]
        );
    }

    #[test]
    fn every_project_client_writes_its_documented_shape() {
        let (_d, root) = root();
        let app = Some("com.x.app");
        let clients = [
            Client::Claude,
            Client::Cursor,
            Client::Vscode,
            Client::Codex,
            Client::Gemini,
            Client::Zed,
            Client::Roo,
        ];
        let reports = setup(&root, app, &opts(&clients), &UserDirs::default()).unwrap();
        assert!(
            reports.iter().all(|r| r.status == Status::Created),
            "{reports:#?}"
        );
        let args = serde_json::json!(["bridge", "--app", "com.x.app"]);
        let claude = json(&root.join(".mcp.json"));
        assert_eq!(claude["mcpServers"]["victauri"]["args"], args);
        assert!(claude["mcpServers"]["victauri"].get("type").is_none());
        let cursor = json(&root.join(".cursor/mcp.json"));
        assert_eq!(cursor["mcpServers"]["victauri"]["type"], "stdio");
        let vscode = json(&root.join(".vscode/mcp.json"));
        assert_eq!(vscode["servers"]["victauri"]["type"], "stdio");
        assert_eq!(vscode["servers"]["victauri"]["args"], args);
        assert_eq!(
            json(&root.join(".gemini/settings.json"))["mcpServers"]["victauri"]["command"],
            "victauri"
        );
        assert_eq!(
            json(&root.join(".zed/settings.json"))["context_servers"]["victauri"]["args"],
            args
        );
        assert_eq!(
            json(&root.join(".roo/mcp.json"))["mcpServers"]["victauri"]["command"],
            "victauri"
        );
        let codex = std::fs::read_to_string(root.join(".codex/config.toml")).unwrap();
        assert!(codex.contains("[mcp_servers.victauri]"), "{codex}");
        assert!(
            codex.contains(r#"args = ["bridge", "--app", "com.x.app"]"#),
            "{codex}"
        );
        // Instructions: CLAUDE.md + skill for Claude, AGENTS.md for the rest, GEMINI.md import.
        assert!(
            std::fs::read_to_string(root.join("CLAUDE.md"))
                .unwrap()
                .contains("VICTAURI:BEGIN")
        );
        assert!(
            std::fs::read_to_string(root.join("AGENTS.md"))
                .unwrap()
                .contains("victauri setup")
        );
        assert!(
            std::fs::read_to_string(root.join("GEMINI.md"))
                .unwrap()
                .contains("@./AGENTS.md")
        );
        let skill = std::fs::read_to_string(root.join(".claude/skills/victauri/SKILL.md")).unwrap();
        assert!(skill.starts_with("---\nname: victauri\ndescription: "));
    }

    #[test]
    fn hostile_identifier_is_one_escaped_argument_everywhere() {
        // A tauri.conf.json identifier is attacker-controllable in a cloned repo: it must stay
        // ONE string argument, never able to inject args, keys or TOML tables.
        let (_d, root) = root();
        let hostile = "x\", \"--evil\"], \"command\": \"sh\\\n[mcp_servers.evil]\ncommand = \"sh\"";
        setup(
            &root,
            Some(hostile),
            &opts(&[Client::Claude, Client::Codex]),
            &UserDirs::default(),
        )
        .unwrap();
        let server = &json(&root.join(".mcp.json"))["mcpServers"]["victauri"];
        assert_eq!(server["command"], "victauri");
        assert_eq!(server["args"].as_array().unwrap().len(), 3);
        assert_eq!(server["args"][2], hostile);
        assert_eq!(server.as_object().unwrap().len(), 2);
        let codex: toml_edit::DocumentMut =
            std::fs::read_to_string(root.join(".codex/config.toml"))
                .unwrap()
                .parse()
                .unwrap();
        let servers = codex["mcp_servers"].as_table().unwrap();
        assert_eq!(servers.len(), 1, "no injected [mcp_servers.evil]");
        assert_eq!(
            servers["victauri"]["args"]
                .as_array()
                .unwrap()
                .get(2)
                .and_then(|v| v.as_str()),
            Some(hostile)
        );
        // The by-hand entry printed for a skipped file is escaped and single-line once shown.
        let manual = manual_entry(
            Format::Json {
                key: "mcpServers",
                typed: false,
            },
            Some("a\"b\n::e"),
        );
        assert!(manual.contains(r#"a\"b\n::e"#), "{manual}");
        assert!(!victauri_test::terminal::single_line(&manual).contains('\n'));
    }

    #[test]
    fn rerun_is_idempotent_byte_for_byte() {
        let (_d, root) = root();
        let all = Client::ALL.to_vec();
        setup(&root, Some("com.x.app"), &opts(&all), &UserDirs::default()).unwrap();
        let snapshot = |root: &Path| {
            let mut files = Vec::new();
            for rel in [
                ".mcp.json",
                ".cursor/mcp.json",
                ".vscode/mcp.json",
                ".codex/config.toml",
                ".gemini/settings.json",
                ".zed/settings.json",
                ".roo/mcp.json",
                "CLAUDE.md",
                "AGENTS.md",
                "GEMINI.md",
                ".claude/skills/victauri/SKILL.md",
            ] {
                files.push(std::fs::read(root.join(rel)).unwrap());
            }
            files
        };
        let before = snapshot(&root);
        let reports = setup(&root, Some("com.x.app"), &opts(&all), &UserDirs::default()).unwrap();
        assert_eq!(snapshot(&root), before);
        assert!(
            reports
                .iter()
                .all(|r| matches!(r.status, Status::Unchanged(_) | Status::Skipped(_))),
            "{reports:#?}"
        );
    }

    #[test]
    fn merges_into_existing_configs_without_clobbering() {
        let (_d, root) = root();
        std::fs::create_dir_all(root.join(".vscode")).unwrap();
        std::fs::write(
            root.join(".vscode/mcp.json"),
            "{\n  // team servers\n  \"inputs\": [],\n  \"servers\": {\n    \"github\": { \"type\": \"http\", \"url\": \"https://api.githubcopilot.com/mcp/\" },\n  },\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"other":{"command":"x"}},"extra":true}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".codex")).unwrap();
        std::fs::write(
            root.join(".codex/config.toml"),
            "# my codex config\nmodel = \"gpt-5\"\n\n[mcp_servers.other]\ncommand = \"x\"\n",
        )
        .unwrap();
        let reports = setup(
            &root,
            None,
            &opts(&[Client::Claude, Client::Vscode, Client::Codex]),
            &UserDirs::default(),
        )
        .unwrap();
        let vs = std::fs::read_to_string(root.join(".vscode/mcp.json")).unwrap();
        assert!(vs.contains("// team servers"), "{vs}");
        let v = json(&root.join(".vscode/mcp.json"));
        assert_eq!(v["servers"]["github"]["type"], "http");
        assert_eq!(
            v["servers"]["victauri"]["args"],
            serde_json::json!(["bridge"])
        );
        let m = json(&root.join(".mcp.json"));
        assert_eq!(m["mcpServers"]["other"]["command"], "x");
        assert_eq!(m["extra"], true);
        let codex = std::fs::read_to_string(root.join(".codex/config.toml")).unwrap();
        assert!(
            codex.starts_with("# my codex config\nmodel = \"gpt-5\""),
            "{codex}"
        );
        assert!(codex.contains("[mcp_servers.other]") && codex.contains("[mcp_servers.victauri]"));
        assert_eq!(
            reports
                .iter()
                .filter(|r| r.status == Status::Updated)
                .count(),
            3,
            "{reports:#?}"
        );
    }

    #[test]
    fn malformed_configs_are_skipped_untouched_with_a_manual_entry() {
        let (_d, root) = root();
        std::fs::create_dir_all(root.join(".cursor")).unwrap();
        std::fs::write(root.join(".cursor/mcp.json"), "{ \"mcpServers\": ").unwrap();
        std::fs::create_dir_all(root.join(".codex")).unwrap();
        std::fs::write(root.join(".codex/config.toml"), "mcp_servers = 3\n").unwrap();
        std::fs::create_dir_all(root.join(".gemini")).unwrap();
        std::fs::write(root.join(".gemini/settings.json"), "[1]").unwrap();
        let reports = setup(
            &root,
            Some("com.x.app"),
            &opts(&[Client::Cursor, Client::Codex, Client::Gemini]),
            &UserDirs::default(),
        )
        .unwrap();
        for r in reports.iter().filter(|r| r.what.contains("MCP")) {
            assert!(matches!(r.status, Status::Skipped(_)), "{r:#?}");
            assert!(r.manual.as_deref().unwrap().contains("com.x.app"));
        }
        assert_eq!(
            std::fs::read_to_string(root.join(".cursor/mcp.json")).unwrap(),
            "{ \"mcpServers\": "
        );
        assert_eq!(
            std::fs::read_to_string(root.join(".codex/config.toml")).unwrap(),
            "mcp_servers = 3\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(".gemini/settings.json")).unwrap(),
            "[1]"
        );
        assert!(matches!(
            merge_codex_toml(Some("not = = toml"), None),
            JsonMerge::Refused(_)
        ));
    }

    #[test]
    fn user_level_clients_need_the_explicit_flag() {
        let (_d, root) = root();
        let (_h, home) = root_home();
        let dirs = UserDirs {
            home: Some(home.clone()),
            appdata: Some(home.join("AppData")),
            xdg_config: None,
            codex_home: None,
        };
        let reports = setup(&root, None, &opts(&[Client::Windsurf]), &dirs).unwrap();
        assert!(
            matches!(&reports[0].status, Status::Skipped(why) if why.contains("--user-config"))
        );
        assert!(
            std::fs::read_dir(&home).unwrap().next().is_none(),
            "nothing written to home"
        );

        let mut o = opts(&[Client::Windsurf, Client::Codex]);
        o.user_config = true;
        let reports = setup(&root, None, &o, &dirs).unwrap();
        let ws = if cfg!(windows) {
            home.join("AppData/devin/mcp_config.json")
        } else {
            home.join(".config/devin/mcp_config.json")
        };
        assert_eq!(json(&ws)["mcpServers"]["victauri"]["command"], "victauri");
        let codex = std::fs::read_to_string(home.join(".codex/config.toml")).unwrap();
        assert!(codex.contains("[mcp_servers.victauri]"));
        assert!(
            !root.join(".codex").exists(),
            "--user-config targets ~/.codex, not the project"
        );
        assert!(
            reports
                .iter()
                .filter(|r| r.what.contains("MCP"))
                .all(|r| r.user_level)
        );
    }

    fn root_home() -> (tempfile::TempDir, PathBuf) {
        root()
    }

    #[test]
    fn legacy_windsurf_config_is_updated_only_if_present() {
        let (_h, home) = root_home();
        let dirs = UserDirs {
            home: Some(home.clone()),
            appdata: Some(home.join("AppData")),
            ..UserDirs::default()
        };
        assert_eq!(dirs.windsurf_configs().len(), 1);
        let legacy = home.join(".codeium/windsurf/mcp_config.json");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "{\"mcpServers\": {}}").unwrap();
        assert_eq!(dirs.windsurf_configs().len(), 2);
        let codex = UserDirs {
            codex_home: Some(home.join("ch")),
            ..dirs
        };
        assert_eq!(
            codex.codex_config().unwrap(),
            home.join("ch").join("config.toml")
        );
    }

    #[test]
    fn dry_run_writes_nothing() {
        let (_d, root) = root();
        let mut o = opts(&Client::ALL);
        o.dry_run = true;
        let reports = setup(&root, None, &o, &UserDirs::default()).unwrap();
        assert!(reports.iter().any(|r| r.status == Status::Created));
        assert!(std::fs::read_dir(&root).unwrap().next().is_none());
    }

    #[test]
    fn no_instructions_writes_only_configs() {
        let (_d, root) = root();
        let mut o = opts(&[Client::Claude, Client::Cursor]);
        o.instructions = false;
        setup(&root, None, &o, &UserDirs::default()).unwrap();
        assert!(root.join(".mcp.json").exists() && root.join(".cursor/mcp.json").exists());
        assert!(!root.join("CLAUDE.md").exists() && !root.join("AGENTS.md").exists());
    }

    #[test]
    fn instruction_blocks_append_once_and_keep_user_text() {
        let (_d, root) = root();
        std::fs::write(root.join("AGENTS.md"), "# My rules\nBe nice.").unwrap();
        setup(&root, None, &opts(&[Client::Cursor]), &UserDirs::default()).unwrap();
        setup(&root, None, &opts(&[Client::Zed]), &UserDirs::default()).unwrap();
        let text = std::fs::read_to_string(root.join("AGENTS.md")).unwrap();
        assert!(text.starts_with("# My rules\nBe nice.\n"));
        assert_eq!(text.matches("VICTAURI:BEGIN").count(), 1);
        assert_eq!(text.matches("VICTAURI:END").count(), 1);
    }

    #[test]
    fn claude_block_is_unchanged_from_earlier_releases() {
        let block = claude_md_block(false);
        assert!(block.starts_with("\n<!-- VICTAURI:BEGIN (added by `victauri init`"));
        assert!(block.contains("\n`.mcp.json` connects through `victauri bridge`"));
        assert!(block.contains("`--app <bundle-identifier>` in `.mcp.json`\n"));
        assert!(block.ends_with("<!-- VICTAURI:END -->\n"));
        assert!(!block.contains("{{"));
        assert!(!block.contains("--features victauri"));
        assert!(!agents_md_block(false).contains("{{"));
    }

    /// `init --optional`: every instruction file says how to start the app WITH Victauri,
    /// inside the marker block (so deleting the block still removes all of it).
    #[test]
    fn feature_gated_note_reaches_every_instruction_file() {
        for text in [claude_md_block(true), agents_md_block(true)] {
            assert!(text.contains("npm run tauri dev -- --features victauri"));
            let note = text.find("--features victauri").unwrap();
            assert!(note < text.find("<!-- VICTAURI:END -->").unwrap());
        }
        assert!(skill_md(true).contains("pnpm tauri dev --features victauri"));
        assert!(!skill_md(false).contains("--features victauri"));

        let (_dir, root) = root();
        let mut o = opts(&[Client::Claude, Client::Cursor]);
        o.feature_gated = true;
        setup(&root, None, &o, &UserDirs::default()).unwrap();
        for f in ["CLAUDE.md", "AGENTS.md", ".claude/skills/victauri/SKILL.md"] {
            let text = std::fs::read_to_string(root.join(f)).unwrap();
            assert!(text.contains("--features victauri"), "{f}");
        }
    }

    #[test]
    fn skill_frontmatter_follows_the_portable_spec() {
        let skill = skill_md(false);
        let fm = skill.split("---\n").nth(1).unwrap();
        let desc = fm
            .lines()
            .find_map(|l| l.strip_prefix("description: "))
            .unwrap();
        assert!(!desc.is_empty() && desc.chars().count() <= 1024);
        assert!(
            !desc.contains(':') && !desc.contains('<'),
            "plain YAML scalar"
        );
        assert!(fm.contains("name: victauri\n"));
    }

    #[cfg(unix)]
    #[test]
    fn never_writes_through_symlinks() {
        let (_d, root) = root();
        let (_o, outside) = root_home();
        std::os::unix::fs::symlink(&outside, root.join(".cursor")).unwrap();
        assert!(setup(&root, None, &opts(&[Client::Cursor]), &UserDirs::default()).is_err());
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
        let target = outside.join("agents.md");
        std::fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, root.join("AGENTS.md")).unwrap();
        let mut o = opts(&[Client::Zed]);
        o.instructions = true;
        assert!(setup(&root, None, &o, &UserDirs::default()).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
    }
}
