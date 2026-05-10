//! CLI for Victauri — scaffold tests, diagnose running apps, and record sessions.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "victauri",
    about = "Full-stack testing toolkit for Tauri apps",
    version,
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scaffold a Victauri test directory with starter tests
    Init {
        /// Path to the Tauri project root (default: current directory)
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// Connect to a running Tauri app and report status
    Check {
        /// Write `JUnit` XML report to this path
        #[arg(long)]
        junit: Option<PathBuf>,
    },
    /// Run the built-in smoke test suite against a running Tauri app
    Test {
        /// Maximum acceptable DOM complete time in milliseconds
        #[arg(long, default_value_t = 10_000)]
        max_load_ms: u64,
        /// Maximum acceptable JS heap usage in megabytes
        #[arg(long, default_value_t = 512.0)]
        max_heap_mb: f64,
        /// Write `JUnit` XML report to this path
        #[arg(long)]
        junit: Option<PathBuf>,
    },
    /// Record user interactions and generate a test file
    Record {
        /// Output file path for generated test
        #[arg(short, long, default_value = "tests/recorded_flow.rs")]
        output: PathBuf,
        /// Name of the generated test function
        #[arg(short = 'n', long, default_value = "recorded_flow")]
        test_name: String,
    },
    /// Watch test files and re-run on changes
    Watch {
        /// Directory to watch (default: tests/)
        #[arg(short, long, default_value = "tests")]
        dir: PathBuf,
        /// Only run tests matching this filter
        #[arg(short, long)]
        filter: Option<String>,
    },
    /// Report IPC command coverage from a running Tauri app
    Coverage {
        /// Minimum coverage percentage — exit with code 1 if below this value
        #[arg(long)]
        threshold: Option<f64>,
        /// Write coverage as `JUnit` XML report to this path
        #[arg(long)]
        junit: Option<PathBuf>,
        /// Allow empty registry (zero commands) without failing
        #[arg(long)]
        allow_empty_registry: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init { path } => {
            let root = path.unwrap_or_else(|| PathBuf::from("."));
            cmd_init(&root)?;
        }
        Commands::Check { junit } => {
            cmd_check(junit.as_deref()).await?;
        }
        Commands::Test {
            max_load_ms,
            max_heap_mb,
            junit,
        } => {
            cmd_test(max_load_ms, max_heap_mb, junit.as_deref()).await?;
        }
        Commands::Record { output, test_name } => {
            cmd_record(&output, &test_name).await?;
        }
        Commands::Watch { dir, filter } => {
            cmd_watch(&dir, filter.as_deref()).await?;
        }
        Commands::Coverage {
            threshold,
            junit,
            allow_empty_registry,
        } => {
            cmd_coverage(threshold, junit.as_deref(), allow_empty_registry).await?;
        }
    }

    Ok(())
}

fn cmd_init(root: &Path) -> Result<()> {
    let root = std::fs::canonicalize(root)
        .with_context(|| format!("directory not found: {}", root.display()))?;

    let (cargo_toml_path, is_tauri) = detect_project(&root)?;

    if !is_tauri {
        eprintln!(
            "Warning: no Tauri dependency detected in {}",
            cargo_toml_path.display()
        );
        eprintln!("         Victauri is designed for Tauri apps — tests may not connect.\n");
    }

    let added = add_dependencies(&cargo_toml_path)?;
    if added {
        eprintln!("  Added victauri-plugin and victauri-test to Cargo.toml");
    } else {
        eprintln!("  Dependencies already present in Cargo.toml");
    }

    let tests_dir = find_src_tauri(&root).map_or_else(|| root.join("tests"), |p| p.join("tests"));
    std::fs::create_dir_all(&tests_dir)
        .with_context(|| format!("failed to create {}", tests_dir.display()))?;

    let smoke_path = tests_dir.join("smoke.rs");
    if smoke_path.exists() {
        eprintln!("  tests/smoke.rs already exists — skipping");
    } else {
        std::fs::write(&smoke_path, generate_smoke_test())
            .with_context(|| format!("failed to write {}", smoke_path.display()))?;
        eprintln!("  Created {}", smoke_path.display());
    }

    eprintln!("\nVictauri initialized. Next steps:");
    eprintln!("  1. Add .plugin(victauri_plugin::init()) to your Tauri builder");
    eprintln!("     (or .plugin(victauri_plugin::init_auto_discover()) for auto-discovery)");
    eprintln!("  2. Start your app:  pnpm run tauri dev");
    eprintln!("  3. Run the tests:   VICTAURI_E2E=1 cargo test --test smoke");
    Ok(())
}

async fn cmd_check(junit_path: Option<&Path>) -> Result<()> {
    eprintln!("Connecting to running Victauri server...\n");

    let mut client = match victauri_test::VictauriClient::discover().await {
        Ok(c) => c,
        Err(e) => {
            bail!(
                "Could not connect to Victauri server: {e}\n\n\
                 Is your Tauri app running? Try:  pnpm run tauri dev\n\
                 The app must have victauri-plugin wired into its builder."
            );
        }
    };

    let info = client
        .get_plugin_info()
        .await
        .context("failed to get plugin info")?;

    let version = info
        .get("version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let tool_count = info
        .get("tool_count")
        .and_then(serde_json::Value::as_u64)
        .or_else(|| info.get("tools").and_then(serde_json::Value::as_u64))
        .map_or("?".to_string(), |n| n.to_string());
    let uptime = info
        .get("uptime_secs")
        .and_then(serde_json::Value::as_u64)
        .map_or("?".to_string(), |s| format!("{s}s"));

    eprintln!("  Victauri v{version}");
    eprintln!("  Tools:  {tool_count}");
    eprintln!("  Uptime: {uptime}");

    let registry = client
        .get_registry()
        .await
        .context("failed to get command registry")?;

    let cmd_count = registry
        .as_array()
        .map(Vec::len)
        .or_else(|| {
            registry
                .get("commands")
                .and_then(|c| c.as_array())
                .map(Vec::len)
        })
        .unwrap_or(0);
    eprintln!("  Registered commands: {cmd_count}");

    let health = client
        .check_ipc_integrity()
        .await
        .context("IPC integrity check failed")?;
    let healthy = health
        .get("healthy")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let stale = health
        .get("stale_calls")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let errors = health
        .get("error_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);

    if healthy {
        eprintln!("  IPC health: OK");
    } else {
        eprintln!("  IPC health: DEGRADED ({stale} stale, {errors} errors)");
    }

    let ghosts = client
        .detect_ghost_commands()
        .await
        .context("ghost command detection failed")?;
    let ghost_list = ghosts
        .get("ghost_commands")
        .and_then(|g| g.as_array())
        .or_else(|| ghosts.get("frontend_only").and_then(|f| f.as_array()));
    if let Some(list) = ghost_list {
        if list.is_empty() {
            eprintln!("  Ghost commands: none");
        } else {
            eprintln!("  Ghost commands: {} detected", list.len());
            for g in list.iter().take(5) {
                if let Some(name) = g.as_str() {
                    eprintln!("    - {name}");
                } else if let Some(name) = g.get("command").and_then(|c| c.as_str()) {
                    eprintln!("    - {name}");
                }
            }
            if list.len() > 5 {
                eprintln!("    ... and {} more", list.len() - 5);
            }
        }
    }

    let mem = client
        .get_memory_stats()
        .await
        .context("memory stats failed")?;
    let rss = mem
        .get("working_set_bytes")
        .or_else(|| mem.get("rss_bytes"))
        .or_else(|| mem.get("rss"))
        .and_then(serde_json::Value::as_u64);
    if let Some(bytes) = rss {
        let mb = bytes as f64 / 1_048_576.0;
        eprintln!("  Memory: {mb:.1} MB");
    }

    eprintln!("\nVictauri server is live and responding.");

    if let Some(path) = junit_path {
        let start = std::time::Instant::now();
        let report = client
            .verify()
            .ipc_healthy()
            .no_console_errors()
            .no_ghost_commands()
            .run()
            .await
            .context("verify checks failed")?;
        let duration = start.elapsed();

        let junit = report.to_junit("victauri-check", duration);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }
        victauri_test::reporting::write_junit_report(&junit, path)
            .with_context(|| format!("failed to write JUnit report to {}", path.display()))?;
        eprintln!("JUnit report written to {}", path.display());
    }

    Ok(())
}

async fn cmd_test(max_load_ms: u64, max_heap_mb: f64, junit_path: Option<&Path>) -> Result<()> {
    eprintln!("Connecting to running Victauri server...\n");

    let mut client = match victauri_test::VictauriClient::discover().await {
        Ok(c) => c,
        Err(e) => {
            bail!(
                "Could not connect to Victauri server: {e}\n\n\
                 Is your Tauri app running? Try:  pnpm run tauri dev\n\
                 The app must have victauri-plugin wired into its builder."
            );
        }
    };

    eprintln!("Running built-in smoke test suite (11 checks)...\n");

    let config = victauri_test::SmokeConfig {
        max_dom_complete_ms: max_load_ms,
        max_heap_mb,
    };

    let report = client
        .smoke_test_with_config(&config)
        .await
        .context("smoke test failed to complete")?;

    eprint!("{}", report.to_summary());

    if let Some(path) = junit_path {
        let verify = report.to_verify_report();
        let junit = verify.to_junit("victauri-smoke", report.duration);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }
        victauri_test::reporting::write_junit_report(&junit, path)
            .with_context(|| format!("failed to write JUnit report to {}", path.display()))?;
        eprintln!("JUnit report written to {}", path.display());
    }

    if !report.all_passed() {
        let fail_count = report.failures().len();
        eprintln!("\n{fail_count} of {} checks failed.", report.total_count());
        std::process::exit(1);
    }

    eprintln!("\nAll smoke checks passed.");
    Ok(())
}

async fn cmd_coverage(
    threshold: Option<f64>,
    junit_path: Option<&Path>,
    allow_empty_registry: bool,
) -> Result<()> {
    eprintln!("Connecting to running Victauri server...\n");

    let mut client = match victauri_test::VictauriClient::discover().await {
        Ok(c) => c,
        Err(e) => {
            bail!(
                "Could not connect to Victauri server: {e}\n\n\
                 Is your Tauri app running? Try:  pnpm run tauri dev\n\
                 The app must have victauri-plugin wired into its builder."
            );
        }
    };

    let report = victauri_test::coverage::coverage_report(&mut client)
        .await
        .context("failed to generate coverage report")?;

    if report.total_commands == 0 && !allow_empty_registry {
        eprintln!("WARNING: No commands registered. Coverage cannot be computed.");
        eprintln!(
            "Hint: Use #[inspectable] on your Tauri commands and call \
             .auto_discover() on VictauriBuilder."
        );
        eprintln!("\nPass --allow-empty-registry to suppress this error.");
        std::process::exit(1);
    }

    let summary = report.to_summary();
    eprintln!("{summary}");

    if let Some(path) = junit_path {
        let verify_report = victauri_test::VerifyReport {
            results: vec![victauri_test::CheckResult {
                description: format!(
                    "IPC coverage {:.1}% ({}/{})",
                    report.coverage_percentage, report.tested_commands, report.total_commands
                ),
                passed: threshold.is_none_or(|t| report.meets_threshold(t)),
                detail: if report.untested.is_empty() {
                    String::new()
                } else {
                    format!("untested: {}", report.untested.join(", "))
                },
            }],
        };
        let junit = verify_report.to_junit("victauri-coverage", std::time::Duration::from_secs(0));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }
        victauri_test::reporting::write_junit_report(&junit, path)
            .with_context(|| format!("failed to write JUnit report to {}", path.display()))?;
        eprintln!("JUnit report written to {}", path.display());
    }

    if let Some(t) = threshold {
        if !report.meets_threshold(t) {
            eprintln!(
                "Coverage {:.1}% is below threshold {:.1}% — failing.",
                report.coverage_percentage, t
            );
            std::process::exit(1);
        }
        eprintln!(
            "Coverage {:.1}% meets threshold {:.1}%.",
            report.coverage_percentage, t
        );
    }

    Ok(())
}

async fn cmd_record(output: &Path, test_name: &str) -> Result<()> {
    eprintln!("Connecting to running Tauri app...\n");

    let mut client = match victauri_test::VictauriClient::discover().await {
        Ok(c) => c,
        Err(e) => {
            bail!(
                "Could not connect to Victauri server: {e}\n\n\
                 Is your Tauri app running? Try:  pnpm run tauri dev\n\
                 The app must have victauri-plugin wired into its builder."
            );
        }
    };

    let session_id = format!("record-{}", uuid::Uuid::new_v4());
    client
        .start_recording(Some(&session_id))
        .await
        .context("failed to start recording")?;

    eprintln!("Recording started (session: {session_id})");
    eprintln!("  Interact with your app — clicks, fills, and key presses are captured.");
    eprintln!("  Press Ctrl+C to stop recording and generate the test.\n");

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let tx = std::sync::Mutex::new(Some(tx));
    ctrlc::set_handler(move || {
        if let Some(tx) = victauri_core::acquire_lock(&tx, "ctrlc_handler").take()
        {
            let _ = tx.send(());
        }
    })
    .context("failed to register Ctrl+C handler")?;

    rx.await.ok();
    eprintln!("\nStopping recording...");

    let session_json = client
        .stop_recording()
        .await
        .context("failed to stop recording")?;

    let session: victauri_core::RecordedSession =
        serde_json::from_value(session_json).context("failed to parse recorded session")?;

    let event_count = session.events.len();
    let interaction_count = session
        .events
        .iter()
        .filter(|e| matches!(e.event, victauri_core::AppEvent::DomInteraction { .. }))
        .count();
    let ipc_count = session
        .events
        .iter()
        .filter(|e| matches!(e.event, victauri_core::AppEvent::Ipc(_)))
        .count();

    let options = victauri_core::CodegenOptions {
        test_name: test_name.to_string(),
        ..victauri_core::CodegenOptions::default()
    };
    let code = victauri_core::generate_test(&session, &options);

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    std::fs::write(output, &code)
        .with_context(|| format!("failed to write {}", output.display()))?;

    eprintln!(
        "  Captured {event_count} events ({interaction_count} interactions, {ipc_count} IPC calls)"
    );
    eprintln!("  Generated test: {}", output.display());
    eprintln!("\nRun your test:");
    eprintln!("  VICTAURI_E2E=1 cargo test --test {test_name}");
    Ok(())
}

async fn cmd_watch(dir: &Path, filter: Option<&str>) -> Result<()> {
    use notify::{RecursiveMode, Watcher};

    let watch_dir = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(dir)
    };

    if !watch_dir.exists() {
        bail!("watch directory does not exist: {}", watch_dir.display());
    }

    eprintln!("Watching {} for changes...", watch_dir.display());
    if let Some(f) = filter {
        eprintln!("  Filter: {f}");
    }
    eprintln!("  Press Ctrl+C to stop.\n");

    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            let dominated_by_rs = event
                .paths
                .iter()
                .any(|p| p.extension().is_some_and(|ext| ext == "rs"));
            if dominated_by_rs {
                let _ = tx.blocking_send(());
            }
        }
    })
    .context("failed to create file watcher")?;

    watcher
        .watch(&watch_dir, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch {}", watch_dir.display()))?;

    run_tests(filter);

    while rx.recv().await.is_some() {
        // Debounce: drain any additional events that arrived during compilation
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        while rx.try_recv().is_ok() {}

        eprintln!("\n--- File changed, re-running tests ---\n");
        run_tests(filter);
    }

    Ok(())
}

fn run_tests(filter: Option<&str>) {
    let start = std::time::Instant::now();
    let mut cmd = std::process::Command::new("cargo");
    cmd.arg("test");
    cmd.env("VICTAURI_E2E", "1");

    if let Some(f) = filter {
        cmd.arg("--test").arg(f);
    }

    let status = cmd.status();
    let elapsed = start.elapsed();

    match status {
        Ok(s) if s.success() => {
            eprintln!("\nAll tests passed ({:.1}s)", elapsed.as_secs_f64());
        }
        Ok(s) => {
            eprintln!(
                "\nTests failed (exit code: {}, {:.1}s)",
                s.code().unwrap_or(-1),
                elapsed.as_secs_f64()
            );
        }
        Err(e) => {
            eprintln!("\nFailed to run cargo test: {e}");
        }
    }

    if elapsed.as_secs() > 30 {
        eprintln!(
            "  Warning: test suite took >{:.0}s — consider splitting slow tests",
            elapsed.as_secs_f64()
        );
    }
}

fn detect_project(root: &Path) -> Result<(PathBuf, bool)> {
    let src_tauri = root.join("src-tauri");
    let cargo_toml = if src_tauri.join("Cargo.toml").exists() {
        src_tauri.join("Cargo.toml")
    } else if root.join("Cargo.toml").exists() {
        root.join("Cargo.toml")
    } else {
        bail!(
            "No Cargo.toml found in {} or {}/src-tauri/",
            root.display(),
            root.display()
        );
    };

    let content = std::fs::read_to_string(&cargo_toml).context("failed to read Cargo.toml")?;
    let is_tauri = content.contains("tauri");

    Ok((cargo_toml, is_tauri))
}

fn find_src_tauri(root: &Path) -> Option<PathBuf> {
    let p = root.join("src-tauri");
    if p.exists() { Some(p) } else { None }
}

fn add_dependencies(cargo_toml_path: &Path) -> Result<bool> {
    let content = std::fs::read_to_string(cargo_toml_path)?;
    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .context("failed to parse Cargo.toml")?;

    let mut changed = false;

    if !has_dep(&doc, "dependencies", "victauri-plugin") {
        ensure_table(&mut doc, "dependencies");
        doc["dependencies"]["victauri-plugin"] = toml_edit::value(env!("CARGO_PKG_VERSION"));
        changed = true;
    }

    if !has_dep(&doc, "dev-dependencies", "victauri-test") {
        ensure_table(&mut doc, "dev-dependencies");
        doc["dev-dependencies"]["victauri-test"] = toml_edit::value(env!("CARGO_PKG_VERSION"));
        changed = true;
    }

    if changed {
        std::fs::write(cargo_toml_path, doc.to_string())?;
    }
    Ok(changed)
}

fn has_dep(doc: &toml_edit::DocumentMut, table: &str, dep: &str) -> bool {
    doc.get(table)
        .and_then(|t| t.as_table())
        .is_some_and(|t| t.contains_key(dep))
}

fn ensure_table(doc: &mut toml_edit::DocumentMut, key: &str) {
    if !doc.contains_key(key) {
        doc[key] = toml_edit::Item::Table(toml_edit::Table::new());
    }
}

fn generate_smoke_test() -> &'static str {
    r#"//! Victauri smoke tests — validates your Tauri app through the MCP bridge.
//!
//! Requires a running Tauri dev server.
//! Run with: VICTAURI_E2E=1 cargo test --test smoke

use victauri_test::VictauriClient;

fn skip_unless_e2e() -> bool {
    if !victauri_test::is_e2e() {
        eprintln!("Skipping: set VICTAURI_E2E=1 with your Tauri dev server running");
        return true;
    }
    false
}

#[tokio::test]
async fn connect_and_check_plugin_info() {
    if skip_unless_e2e() {
        return;
    }

    let mut client = VictauriClient::discover()
        .await
        .expect("Failed to connect — is your Tauri dev server running?");

    let info = client.get_plugin_info().await.unwrap();
    assert!(
        info.get("version").is_some(),
        "plugin_info should have version"
    );
    eprintln!("Connected to Victauri v{}", info["version"]);
}

#[tokio::test]
async fn screenshot_captures_window() {
    if skip_unless_e2e() {
        return;
    }

    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.screenshot().await.unwrap();

    let has_image = result.get("image").is_some()
        || result.get("data").is_some()
        || result.get("base64").is_some()
        || result.pointer("/result/content/0/data").is_some();
    assert!(has_image, "screenshot should return image data");
    eprintln!("Screenshot captured successfully");
}

#[tokio::test]
async fn ipc_integrity_passes() {
    if skip_unless_e2e() {
        return;
    }

    let mut client = VictauriClient::discover().await.unwrap();
    let report = client
        .verify()
        .ipc_healthy()
        .no_console_errors()
        .run()
        .await
        .unwrap();

    for result in &report.results {
        eprintln!(
            "  [{}] {}",
            if result.passed { "PASS" } else { "FAIL" },
            result.description,
        );
    }
    assert!(
        report.all_passed(),
        "IPC integrity checks should pass: {:?}",
        report
            .failures()
            .iter()
            .map(|f| &f.description)
            .collect::<Vec<_>>()
    );
}
"#
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn smoke_test_content_compiles_as_valid_rust() {
        let content = generate_smoke_test();
        assert!(content.contains("VictauriClient"));
        assert!(content.contains("skip_unless_e2e"));
        assert!(content.contains("#[tokio::test]"));
        assert!(content.contains("VICTAURI_E2E=1"));
    }

    #[test]
    fn detect_project_finds_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        let cargo = dir.path().join("Cargo.toml");
        let mut f = std::fs::File::create(&cargo).unwrap();
        writeln!(f, "[dependencies]\ntauri = \"2\"").unwrap();

        let (path, is_tauri) = detect_project(dir.path()).unwrap();
        assert_eq!(path, cargo);
        assert!(is_tauri);
    }

    #[test]
    fn detect_project_finds_src_tauri() {
        let dir = tempfile::tempdir().unwrap();
        let src_tauri = dir.path().join("src-tauri");
        std::fs::create_dir_all(&src_tauri).unwrap();
        let cargo = src_tauri.join("Cargo.toml");
        let mut f = std::fs::File::create(&cargo).unwrap();
        writeln!(f, "[dependencies]\ntauri = \"2\"").unwrap();

        let (path, is_tauri) = detect_project(dir.path()).unwrap();
        assert_eq!(path, cargo);
        assert!(is_tauri);
    }

    #[test]
    fn detect_project_not_tauri() {
        let dir = tempfile::tempdir().unwrap();
        let cargo = dir.path().join("Cargo.toml");
        let mut f = std::fs::File::create(&cargo).unwrap();
        writeln!(f, "[dependencies]\nserde = \"1\"").unwrap();

        let (_, is_tauri) = detect_project(dir.path()).unwrap();
        assert!(!is_tauri);
    }

    #[test]
    fn detect_project_missing_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        assert!(detect_project(dir.path()).is_err());
    }

    #[test]
    fn add_deps_creates_sections() {
        let dir = tempfile::tempdir().unwrap();
        let cargo = dir.path().join("Cargo.toml");
        std::fs::write(
            &cargo,
            "[package]\nname = \"test-app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let added = add_dependencies(&cargo).unwrap();
        assert!(added);

        let content = std::fs::read_to_string(&cargo).unwrap();
        assert!(content.contains("victauri-plugin"));
        assert!(content.contains("victauri-test"));
    }

    #[test]
    fn add_deps_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cargo = dir.path().join("Cargo.toml");
        std::fs::write(
            &cargo,
            "[package]\nname = \"test-app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        add_dependencies(&cargo).unwrap();
        let second = add_dependencies(&cargo).unwrap();
        assert!(!second, "second call should report no changes");
    }
}
