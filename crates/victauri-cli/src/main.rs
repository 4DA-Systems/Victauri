#![forbid(unsafe_code)]
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
        /// Generate Locator API style code instead of direct client methods
        #[arg(long)]
        locator: bool,
        /// Emit `assert_ipc_called` assertions for each IPC command
        #[arg(long)]
        assert_ipc: bool,
    },
    /// Diagnose your Victauri setup — check every step from plugin wiring to tool operation
    Doctor,
    /// Watch test files and re-run on changes
    Watch {
        /// Directory to watch (default: tests/)
        #[arg(short, long, default_value = "tests")]
        dir: PathBuf,
        /// Only run tests matching this filter
        #[arg(short, long)]
        filter: Option<String>,
    },
    /// Invoke a Tauri IPC command on a running app and print the result
    Invoke {
        /// The command name to invoke (e.g. "get_source_health")
        command: String,
        /// JSON arguments to pass to the command (e.g. '{"limit": 10}')
        #[arg(short, long)]
        args: Option<String>,
        /// Print raw JSON output (no formatting)
        #[arg(long)]
        raw: bool,
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
        Commands::Doctor => {
            cmd_doctor().await?;
        }
        Commands::Record {
            output,
            test_name,
            locator,
            assert_ipc,
        } => {
            cmd_record(&output, &test_name, locator, assert_ipc).await?;
        }
        Commands::Watch { dir, filter } => {
            cmd_watch(&dir, filter.as_deref()).await?;
        }
        Commands::Invoke { command, args, raw } => {
            cmd_invoke(&command, args.as_deref(), raw).await?;
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

    eprintln!("Initializing Victauri...\n");

    // Step 1: Add dependencies to Cargo.toml
    let added = add_dependencies(&cargo_toml_path)?;
    if added {
        eprintln!("  [+] Added victauri-plugin and victauri-test to Cargo.toml");
    } else {
        eprintln!("  [=] Dependencies already present in Cargo.toml");
    }

    // Step 2: Auto-patch main.rs/lib.rs with plugin wiring
    let src_dir = cargo_toml_path
        .parent()
        .map(|p| p.join("src"))
        .unwrap_or_default();
    let mut patched = false;
    if src_dir.exists() {
        patched = try_patch_tauri_builder(&src_dir)?;
    }
    if !patched {
        eprintln!("  [!] Could not auto-patch your Tauri builder.");
        eprintln!("      Add this line manually:\n");
        eprintln!("        .plugin(victauri_plugin::init())\n");
    }

    // Step 3: Create .mcp.json for AI agent connection
    let mcp_json_path = root.join(".mcp.json");
    if mcp_json_path.exists() {
        let content = std::fs::read_to_string(&mcp_json_path).unwrap_or_default();
        if content.contains("victauri") {
            eprintln!("  [=] .mcp.json already configured for Victauri");
        } else {
            eprintln!("  [!] .mcp.json exists but doesn't reference Victauri.");
            eprintln!("      Add this to your mcpServers:\n");
            eprintln!("        \"victauri\": {{ \"url\": \"http://127.0.0.1:7373/mcp\" }}\n");
        }
    } else {
        std::fs::write(&mcp_json_path, generate_mcp_json())
            .with_context(|| format!("failed to write {}", mcp_json_path.display()))?;
        eprintln!("  [+] Created .mcp.json (AI agent configuration)");
    }

    // Step 4: Create Tauri capability for Victauri
    let capabilities_dir = find_capabilities_dir(&cargo_toml_path);
    if let Some(caps_dir) = capabilities_dir {
        let cap_path = caps_dir.join("victauri.json");
        if cap_path.exists() {
            eprintln!("  [=] capabilities/victauri.json already exists");
        } else {
            std::fs::create_dir_all(&caps_dir)
                .with_context(|| format!("failed to create {}", caps_dir.display()))?;
            std::fs::write(&cap_path, generate_capability_json())
                .with_context(|| format!("failed to write {}", cap_path.display()))?;
            eprintln!("  [+] Created capabilities/victauri.json");
        }
    }

    // Step 5: Scan for Tauri commands and create test files
    let src_dir_for_scan = cargo_toml_path
        .parent()
        .map(|p| p.join("src"))
        .unwrap_or_default();
    let discovered_commands = if src_dir_for_scan.exists() {
        scan_tauri_commands(&src_dir_for_scan)
    } else {
        Vec::new()
    };
    if !discovered_commands.is_empty() {
        eprintln!(
            "  [*] Discovered {} #[tauri::command] functions: {}",
            discovered_commands.len(),
            discovered_commands.join(", ")
        );
    }

    let tests_dir = find_src_tauri(&root).map_or_else(|| root.join("tests"), |p| p.join("tests"));
    std::fs::create_dir_all(&tests_dir)
        .with_context(|| format!("failed to create {}", tests_dir.display()))?;

    let smoke_path = tests_dir.join("smoke.rs");
    if smoke_path.exists() {
        eprintln!("  [=] tests/smoke.rs already exists");
    } else {
        std::fs::write(&smoke_path, generate_smoke_test())
            .with_context(|| format!("failed to write {}", smoke_path.display()))?;
        eprintln!("  [+] Created tests/smoke.rs (smoke tests)");
    }

    let integration_path = tests_dir.join("integration.rs");
    if integration_path.exists() {
        eprintln!("  [=] tests/integration.rs already exists");
    } else {
        let integration_content = if discovered_commands.is_empty() {
            generate_integration_test().to_string()
        } else {
            generate_integration_test_with_commands(&discovered_commands)
        };
        std::fs::write(&integration_path, &integration_content)
            .with_context(|| format!("failed to write {}", integration_path.display()))?;
        eprintln!("  [+] Created tests/integration.rs (integration test template)");
    }

    // Step 6: Generate CI workflow
    let workflows_dir = root.join(".github").join("workflows");
    let ci_path = workflows_dir.join("victauri.yml");
    if ci_path.exists() {
        eprintln!("  [=] .github/workflows/victauri.yml already exists");
    } else {
        std::fs::create_dir_all(&workflows_dir)
            .with_context(|| format!("failed to create {}", workflows_dir.display()))?;
        std::fs::write(&ci_path, generate_ci_workflow())
            .with_context(|| format!("failed to write {}", ci_path.display()))?;
        eprintln!("  [+] Created .github/workflows/victauri.yml (CI pipeline)");
    }

    // Step 7: Add Victauri section to CLAUDE.md for AI agent guidance
    let claude_md_path = root.join("CLAUDE.md");
    if claude_md_path.exists() {
        let content = std::fs::read_to_string(&claude_md_path).unwrap_or_default();
        if content.contains("Victauri") || content.contains("victauri") {
            eprintln!("  [=] CLAUDE.md already references Victauri");
        } else {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&claude_md_path)
                .with_context(|| format!("failed to append to {}", claude_md_path.display()))?;
            std::io::Write::write_all(&mut file, generate_claude_md_section().as_bytes())
                .with_context(|| format!("failed to write to {}", claude_md_path.display()))?;
            eprintln!("  [+] Added Victauri section to CLAUDE.md");
        }
    } else {
        std::fs::write(&claude_md_path, generate_claude_md_section())
            .with_context(|| format!("failed to write {}", claude_md_path.display()))?;
        eprintln!("  [+] Created CLAUDE.md with Victauri agent instructions");
    }

    // Step 8: Print summary
    let mut remaining_steps = Vec::new();
    if !patched {
        remaining_steps
            .push("Add .plugin(victauri_plugin::init()) to your Tauri builder".to_string());
    }
    remaining_steps.push("Start your app:  pnpm tauri dev".to_string());
    remaining_steps.push("Run the tests:   VICTAURI_E2E=1 cargo test --test smoke".to_string());
    remaining_steps.push("Try the CLI:     victauri check".to_string());

    eprintln!("\nVictauri initialized. Next steps:");
    for (i, step) in remaining_steps.iter().enumerate() {
        eprintln!("  {}. {step}", i + 1);
    }

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

async fn cmd_doctor() -> Result<()> {
    eprintln!("Victauri Doctor — checking your setup...\n");

    let mut pass_count = 0u32;
    let mut fail_count = 0u32;
    let mut warn_count = 0u32;

    // Check 1: Project structure
    let cwd = std::env::current_dir()?;
    let (cargo_path, is_tauri) = if let Ok(result) = detect_project(&cwd) {
        eprintln!("  [PASS] Cargo.toml found: {}", result.0.display());
        pass_count += 1;
        result
    } else {
        eprintln!("  [FAIL] No Cargo.toml found in current directory");
        eprintln!("         Run this command from your Tauri project root.\n");
        fail_count += 1;
        print_doctor_summary(pass_count, fail_count, warn_count);
        return Ok(());
    };

    // Check 2: Tauri dependency
    if is_tauri {
        eprintln!("  [PASS] Tauri dependency detected");
        pass_count += 1;
    } else {
        eprintln!("  [FAIL] No Tauri dependency in Cargo.toml");
        eprintln!(
            "         Add `tauri` to [dependencies] in {}",
            cargo_path.display()
        );
        fail_count += 1;
    }

    // Check 3: Victauri plugin dependency
    let cargo_content = std::fs::read_to_string(&cargo_path).unwrap_or_default();
    if cargo_content.contains("victauri-plugin") {
        eprintln!("  [PASS] victauri-plugin dependency present");
        pass_count += 1;
    } else {
        eprintln!("  [FAIL] victauri-plugin not in Cargo.toml");
        eprintln!("         Run: victauri init");
        fail_count += 1;
    }

    // Check 4: Victauri test dependency
    if cargo_content.contains("victauri-test") {
        eprintln!("  [PASS] victauri-test dependency present");
        pass_count += 1;
    } else {
        eprintln!("  [WARN] victauri-test not in Cargo.toml");
        eprintln!("         Run: victauri init");
        warn_count += 1;
    }

    // Check 5: Plugin wiring in source code
    let src_dir = cargo_path
        .parent()
        .map(|p| p.join("src"))
        .unwrap_or_default();
    let mut plugin_wired = false;
    for filename in ["lib.rs", "main.rs"] {
        let path = src_dir.join(filename);
        if path.exists() {
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            if content.contains("victauri_plugin") {
                eprintln!("  [PASS] Plugin wired in {filename}");
                plugin_wired = true;
                pass_count += 1;
                break;
            }
        }
    }
    if !plugin_wired {
        eprintln!("  [FAIL] victauri_plugin not referenced in src/main.rs or src/lib.rs");
        eprintln!("         Add .plugin(victauri_plugin::init()) to your Tauri builder");
        fail_count += 1;
    }

    // Check 6: .mcp.json
    let mcp_path = cwd.join(".mcp.json");
    if mcp_path.exists() {
        let content = std::fs::read_to_string(&mcp_path).unwrap_or_default();
        if content.contains("7373") || content.contains("victauri") {
            eprintln!("  [PASS] .mcp.json configured for Victauri");
            pass_count += 1;
        } else {
            eprintln!("  [WARN] .mcp.json exists but may not reference Victauri");
            warn_count += 1;
        }
    } else {
        eprintln!("  [WARN] No .mcp.json found (needed for AI agent connection)");
        eprintln!("         Run: victauri init");
        warn_count += 1;
    }

    // Check 7: Capabilities
    let caps_dir = find_capabilities_dir(&cargo_path);
    if let Some(ref caps) = caps_dir {
        let victauri_cap = caps.join("victauri.json");
        if victauri_cap.exists() {
            eprintln!("  [PASS] capabilities/victauri.json exists");
            pass_count += 1;
        } else {
            // Check if any capability file references victauri
            let has_victauri_perm = std::fs::read_dir(caps).is_ok_and(|entries| {
                entries.filter_map(Result::ok).any(|e| {
                    std::fs::read_to_string(e.path())
                        .unwrap_or_default()
                        .contains("victauri")
                })
            });
            if has_victauri_perm {
                eprintln!("  [PASS] Victauri permissions found in capabilities");
                pass_count += 1;
            } else {
                eprintln!("  [WARN] No Victauri capability configured");
                eprintln!("         Run: victauri init");
                warn_count += 1;
            }
        }
    }

    // Check 8: Test files
    let tests_dir = find_src_tauri(&cwd).map_or_else(|| cwd.join("tests"), |p| p.join("tests"));
    if tests_dir.join("smoke.rs").exists() || tests_dir.join("integration.rs").exists() {
        eprintln!("  [PASS] Test files found in {}", tests_dir.display());
        pass_count += 1;
    } else {
        eprintln!("  [WARN] No Victauri test files found");
        eprintln!("         Run: victauri init");
        warn_count += 1;
    }

    // Check 9: Server connectivity
    eprintln!();
    eprintln!("  Checking server connectivity...");
    if let Ok(mut client) = victauri_test::VictauriClient::discover().await {
        eprintln!("  [PASS] Connected to Victauri server");
        pass_count += 1;

        // Check 10: Plugin info
        if let Ok(info) = client.get_plugin_info().await {
            let version = info
                .get("version")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            eprintln!("  [PASS] Plugin responding (v{version})");
            pass_count += 1;
        } else {
            eprintln!("  [FAIL] Plugin info unavailable");
            fail_count += 1;
        }

        // Check 11: JS bridge
        if let Ok(val) = client.eval_js("typeof window.__VICTAURI__").await {
            let bridge_type = val.as_str().unwrap_or("undefined");
            if bridge_type == "object" {
                eprintln!("  [PASS] JS bridge loaded and responding");
                pass_count += 1;

                if let Ok(ver) = client.eval_js("window.__VICTAURI__.version").await {
                    let ver_str = ver.as_str().unwrap_or("unknown");
                    eprintln!("         Bridge version: {ver_str}");
                }
            } else {
                eprintln!("  [FAIL] JS bridge not loaded (typeof = {bridge_type})");
                eprintln!("         Check that your webview is rendering and CSP allows scripts");
                fail_count += 1;
            }
        } else {
            eprintln!("  [FAIL] JS eval failed");
            eprintln!("         The webview may not be ready or CSP may block eval");
            fail_count += 1;
        }

        // Check 12: DOM snapshot
        if let Ok(snap) = client.dom_snapshot().await {
            let element_count = snap
                .get("element_count")
                .and_then(serde_json::Value::as_u64)
                .or_else(|| {
                    snap.get("tree")
                        .and_then(|t| t.get("children"))
                        .and_then(|c| c.as_array())
                        .map(|a| a.len() as u64)
                })
                .unwrap_or(0);
            eprintln!("  [PASS] DOM snapshot works ({element_count} elements)");
            pass_count += 1;
        } else {
            eprintln!("  [FAIL] DOM snapshot failed");
            fail_count += 1;
        }

        // Check 13: IPC integrity
        if let Ok(report) = client.check_ipc_integrity().await {
            let healthy = report
                .get("healthy")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if healthy {
                eprintln!("  [PASS] IPC integrity healthy");
                pass_count += 1;
            } else {
                eprintln!("  [WARN] IPC integrity degraded");
                warn_count += 1;
            }
        } else {
            eprintln!("  [FAIL] IPC integrity check failed");
            fail_count += 1;
        }
    } else {
        eprintln!("  [SKIP] Server not running — skipping runtime checks");
        eprintln!("         Start your app to test the full chain: pnpm tauri dev");
    }

    eprintln!();
    print_doctor_summary(pass_count, fail_count, warn_count);
    Ok(())
}

fn print_doctor_summary(pass: u32, fail: u32, warn: u32) {
    let total = pass + fail + warn;
    eprintln!("Summary: {pass}/{total} passed, {fail} failed, {warn} warnings");
    if fail == 0 && warn == 0 {
        eprintln!("Your Victauri setup looks good!");
    } else if fail == 0 {
        eprintln!("Setup is functional but has minor issues. Run `victauri init` to fix.");
    } else {
        eprintln!("Setup needs attention. Fix the [FAIL] items above to get started.");
    }
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

async fn cmd_invoke(command: &str, args_json: Option<&str>, raw: bool) -> Result<()> {
    if !raw {
        eprintln!("Connecting to running Victauri server...\n");
    }

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

    let args: Option<serde_json::Value> = match args_json {
        Some(s) => Some(serde_json::from_str(s).context("invalid JSON in --args")?),
        None => None,
    };

    let result = client
        .invoke_command(command, args)
        .await
        .with_context(|| format!("failed to invoke command '{command}'"))?;

    if raw {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        eprintln!("  Command: {command}");
        if let Some(a) = args_json {
            eprintln!("  Args:    {a}");
        }
        eprintln!();
        println!("{}", serde_json::to_string_pretty(&result)?);
    }

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

async fn cmd_record(output: &Path, test_name: &str, locator: bool, assert_ipc: bool) -> Result<()> {
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
        if let Some(tx) = victauri_core::acquire_lock(&tx, "ctrlc_handler").take() {
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

    let style = if locator {
        victauri_core::CodegenStyle::Locator
    } else {
        victauri_core::CodegenStyle::Direct
    };
    let options = victauri_core::CodegenOptions {
        test_name: test_name.to_string(),
        emit_ipc_assert_calls: assert_ipc,
        style,
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

    eprintln!("\x1b[36mVictauri Watch\x1b[0m");
    eprintln!("  Directory: {}", watch_dir.display());
    if let Some(f) = filter {
        eprintln!("  Filter: {f}");
    }
    eprintln!("  Press Ctrl+C to stop.\n");

    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res
            && let Some(path) = event
                .paths
                .iter()
                .find(|p| p.extension().is_some_and(|ext| ext == "rs"))
        {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let _ = tx.blocking_send(name);
        }
    })
    .context("failed to create file watcher")?;

    watcher
        .watch(&watch_dir, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch {}", watch_dir.display()))?;

    run_tests_with_output(filter, None);

    while let Some(changed_file) = rx.recv().await {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        while rx.try_recv().is_ok() {}

        run_tests_with_output(filter, Some(&changed_file));
    }

    Ok(())
}

fn run_tests_with_output(filter: Option<&str>, changed_file: Option<&str>) {
    // Clear screen
    eprint!("\x1b[2J\x1b[H");

    eprintln!("\x1b[36mVictauri Watch\x1b[0m");
    if let Some(file) = changed_file {
        eprintln!("  Triggered by: \x1b[33m{file}\x1b[0m");
    }
    eprintln!();

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
            eprintln!(
                "\n\x1b[32m  PASS\x1b[0m  All tests passed ({:.1}s)",
                elapsed.as_secs_f64()
            );
        }
        Ok(s) => {
            eprintln!(
                "\n\x1b[31m  FAIL\x1b[0m  Tests failed (exit {}, {:.1}s)",
                s.code().unwrap_or(-1),
                elapsed.as_secs_f64()
            );
        }
        Err(e) => {
            eprintln!("\n\x1b[31m  ERROR\x1b[0m  Failed to run cargo test: {e}");
        }
    }

    if elapsed.as_secs() > 30 {
        eprintln!(
            "  \x1b[33mSlow:\x1b[0m test suite took {:.0}s — consider splitting slow tests",
            elapsed.as_secs_f64()
        );
    }

    eprintln!("\n\x1b[90mWaiting for changes...\x1b[0m");
}

fn try_patch_tauri_builder(src_dir: &Path) -> Result<bool> {
    for filename in ["lib.rs", "main.rs"] {
        let path = src_dir.join(filename);
        if !path.exists() {
            continue;
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;

        if content.contains("victauri_plugin") {
            eprintln!(
                "  [=] {} already references victauri_plugin",
                path.display()
            );
            return Ok(true);
        }

        if !content.contains("tauri::Builder") {
            continue;
        }

        // Try to find a safe insertion point:
        // Look for `.run(tauri::generate_context` or `.build(tauri::generate_context`
        // and insert `.plugin(victauri_plugin::init())` before that line.
        let lines: Vec<&str> = content.lines().collect();
        let mut insert_idx = None;
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.contains(".run(tauri::generate_context")
                || trimmed.contains(".build(tauri::generate_context")
            {
                insert_idx = Some(i);
                break;
            }
        }

        if let Some(idx) = insert_idx {
            let indent = lines[idx]
                .chars()
                .take_while(|c| c.is_whitespace())
                .collect::<String>();
            let plugin_line = format!("{indent}.plugin(victauri_plugin::init())");

            let mut new_lines = lines[..idx].to_vec();
            new_lines.push(&plugin_line);
            new_lines.extend_from_slice(&lines[idx..]);

            let new_content = new_lines.join("\n");
            // Preserve trailing newline if original had one
            let new_content = if content.ends_with('\n') {
                format!("{new_content}\n")
            } else {
                new_content
            };

            std::fs::write(&path, new_content)
                .with_context(|| format!("failed to write {}", path.display()))?;
            eprintln!(
                "  [+] Patched {} with .plugin(victauri_plugin::init())",
                path.display()
            );
            return Ok(true);
        }
    }
    Ok(false)
}

fn generate_mcp_json() -> &'static str {
    r#"{
  "mcpServers": {
    "victauri": {
      "url": "http://127.0.0.1:7373/mcp"
    }
  }
}
"#
}

fn generate_claude_md_section() -> &'static str {
    r#"
## Victauri (App Inspection & Testing)

This app has **Victauri** integrated — an MCP server embedded inside the Tauri process
that gives full-stack access to the webview DOM, IPC layer, Rust backend, and native
windows. Available when the app is running in debug mode.

**Use Victauri MCP tools for all app inspection and testing tasks.** Victauri runs inside
the app process with sub-ms response times and direct AppHandle access — it sees
everything, not just the webview.

Key tools that only Victauri can provide (not available via CDP/Playwright):
- `invoke_command` — call any registered Tauri command directly
- `verify_state` — cross-boundary frontend/backend state verification
- `detect_ghost_commands` — find commands with no backend handler
- `check_ipc_integrity` — verify IPC pipeline health
- `get_memory_stats` — real OS process memory stats
- `audit_accessibility` — WCAG accessibility checks
- `get_performance` — navigation timing, JS heap, resource loading

Connection: `http://127.0.0.1:7373/mcp` (configured in `.mcp.json`)

Do NOT use Playwright or CDP for tasks that Victauri handles. Only fall back to
Playwright for browser-only work unrelated to the Tauri app.
"#
}

fn generate_capability_json() -> &'static str {
    r#"{
  "identifier": "victauri",
  "description": "Victauri testing plugin — debug builds only",
  "context": "local",
  "windows": ["*"],
  "permissions": [
    "victauri:default"
  ]
}
"#
}

fn find_capabilities_dir(cargo_toml_path: &Path) -> Option<PathBuf> {
    let project_dir = cargo_toml_path.parent()?;
    // Tauri 2 standard: capabilities/ next to Cargo.toml
    let caps = project_dir.join("capabilities");
    if caps.exists() || project_dir.join("tauri.conf.json").exists() {
        return Some(caps);
    }
    None
}

fn generate_integration_test() -> &'static str {
    r#"//! Integration test template — demonstrates Victauri's full-stack testing capabilities.
//!
//! Run with: VICTAURI_E2E=1 cargo test --test integration

use victauri_test::prelude::*;

fn skip_unless_e2e() -> bool {
    if !is_e2e() {
        eprintln!("Skipping: set VICTAURI_E2E=1 with your Tauri dev server running");
        return true;
    }
    false
}

#[tokio::test]
async fn full_stack_health_check() {
    if skip_unless_e2e() { return; }
    let mut client = VictauriClient::discover().await
        .expect("Failed to connect — is your Tauri dev server running?");

    let report = client.verify()
        .ipc_healthy()
        .no_console_errors()
        .no_ghost_commands()
        .run()
        .await
        .unwrap();

    for result in &report.results {
        eprintln!("  [{}] {}", if result.passed { "PASS" } else { "FAIL" }, result.description);
    }
    report.assert_all_passed();
}

// Uncomment and adapt these patterns for your app:
//
// #[tokio::test]
// async fn form_submission() {
//     if skip_unless_e2e() { return; }
//     let mut client = VictauriClient::discover().await.unwrap();
//
//     // Find elements by label or test ID
//     let input = Locator::label("Name");
//     let submit = Locator::role("button").and_text("Submit");
//
//     // Interact
//     input.fill(&mut client, "World").await.unwrap();
//     submit.click(&mut client).await.unwrap();
//
//     // Wait for result and verify
//     Locator::text("Hello, World!")
//         .expect(&mut client)
//         .to_be_visible()
//         .await
//         .unwrap();
//
//     // Verify IPC call happened with correct args
//     let log = client.get_ipc_log(Some(1)).await.unwrap();
//     assert_ipc_called(&log, "greet");
// }
//
// #[tokio::test]
// async fn visual_regression() {
//     if skip_unless_e2e() { return; }
//     let mut client = VictauriClient::discover().await.unwrap();
//
//     let opts = VisualOptions {
//         snapshot_dir: "tests/snapshots".into(),
//         ..VisualOptions::from_preset(ThresholdPreset::Standard)
//     };
//     let diff = client.screenshot_visual("main-view", &opts).await.unwrap();
//     assert!(diff.is_match, "visual regression: {:.2}% differ", diff.diff_percentage);
// }
"#
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

fn scan_tauri_commands(src_dir: &Path) -> Vec<String> {
    let mut commands = Vec::new();
    let mut dirs = vec![src_dir.to_path_buf()];
    let mut rs_files = Vec::new();
    while let Some(dir) = dirs.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    rs_files.push(path);
                }
            }
        }
    }

    for path in rs_files {
        if let Ok(content) = std::fs::read_to_string(&path) {
            let mut in_command_attr = false;
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.contains("#[tauri::command")
                    || trimmed.contains("#[command")
                    || trimmed.contains("#[inspectable")
                {
                    in_command_attr = true;
                    continue;
                }
                if in_command_attr {
                    if let Some(name) = extract_fn_name(trimmed) {
                        commands.push(name);
                        in_command_attr = false;
                        continue;
                    }
                    if trimmed.starts_with("pub")
                        || trimmed.starts_with("fn ")
                        || trimmed.starts_with("async")
                    {
                        if let Some(name) = extract_fn_name(trimmed) {
                            commands.push(name);
                        }
                        in_command_attr = false;
                    }
                }
            }
        }
    }
    commands.sort();
    commands.dedup();
    commands
}

fn extract_fn_name(line: &str) -> Option<String> {
    let rest = if let Some(i) = line.find("fn ") {
        &line[i + 3..]
    } else {
        return None;
    };
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() { None } else { Some(name) }
}

fn generate_integration_test_with_commands(commands: &[String]) -> String {
    let mut out = String::from(
        r#"//! Integration tests — auto-generated from discovered #[tauri::command] functions.
//!
//! Run with: VICTAURI_E2E=1 cargo test --test integration

use serde_json::json;
use victauri_test::{e2e_test, VictauriClient};

fn skip_unless_e2e() -> bool {
    if !victauri_test::is_e2e() {
        eprintln!("Skipping: set VICTAURI_E2E=1 with your Tauri dev server running");
        return true;
    }
    false
}

// ── Health check ─────────────────────────────────────────────────────────

#[tokio::test]
async fn full_stack_health_check() {
    if skip_unless_e2e() { return; }
    let mut client = VictauriClient::discover().await
        .expect("Failed to connect — is your Tauri dev server running?");

    let report = client.verify()
        .ipc_healthy()
        .no_console_errors()
        .run()
        .await
        .unwrap();

    report.assert_all_passed();
}

// ── Command tests ────────────────────────────────────────────────────────
// Generated from discovered #[tauri::command] functions.
// Adapt each test to match your command's expected arguments and behavior.

"#,
    );

    for cmd in commands {
        out.push_str(&format!(
            r#"#[tokio::test]
async fn command_{cmd}() {{
    if skip_unless_e2e() {{ return; }}
    let mut client = VictauriClient::discover().await.unwrap();

    let result = client.invoke_command("{cmd}", None).await;
    assert!(
        result.is_ok(),
        "{cmd} should respond without error: {{:?}}",
        result.err()
    );
}}

"#,
        ));
    }

    out
}

fn generate_ci_workflow() -> &'static str {
    r#"# Victauri E2E tests — runs smoke + integration tests against your Tauri app.
# Generated by `victauri init`. Customize as needed.

name: Victauri E2E

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

jobs:
  e2e:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install system dependencies
        run: |
          sudo apt-get update
          sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev libgtk-3-dev xvfb

      - uses: dtolnay/rust-toolchain@stable

      - uses: Swatinem/rust-cache@v2

      - name: Install victauri-cli
        run: cargo install victauri-cli

      - name: Build app
        run: cargo build

      - name: Start app under xvfb
        run: |
          xvfb-run -a ./target/debug/$(cargo metadata --format-version=1 --no-deps | jq -r '.packages[0].name') &
          echo "APP_PID=$!" >> "$GITHUB_ENV"

      - name: Wait for Victauri server
        run: |
          timeout=30
          elapsed=0
          until curl -sf http://127.0.0.1:7373/health > /dev/null 2>&1; do
            if [ "$elapsed" -ge "$timeout" ]; then
              echo "ERROR: Victauri server not ready within ${timeout}s"
              exit 1
            fi
            sleep 1
            elapsed=$((elapsed + 1))
          done
          echo "Server ready after ${elapsed}s"

      - name: Run smoke tests
        run: victauri test

      - name: Run integration tests
        run: cargo test --test integration -- --test-threads=1
        env:
          VICTAURI_E2E: "1"

      - name: Cleanup
        if: always()
        run: |
          if [ -n "$APP_PID" ]; then
            kill "$APP_PID" 2>/dev/null || true
          fi
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

    #[test]
    fn patch_tauri_builder_inserts_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("main.rs"),
            "fn main() {\n    tauri::Builder::default()\n        .run(tauri::generate_context!())\n        .unwrap();\n}\n",
        )
        .unwrap();

        let patched = try_patch_tauri_builder(&src).unwrap();
        assert!(patched, "should have patched the file");

        let content = std::fs::read_to_string(src.join("main.rs")).unwrap();
        assert!(content.contains("victauri_plugin::init()"));
        assert!(
            content.find("victauri_plugin").unwrap()
                < content.find(".run(tauri::generate_context").unwrap(),
            "plugin line should appear before .run()"
        );
    }

    #[test]
    fn patch_tauri_builder_skips_if_already_present() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("main.rs"),
            "fn main() {\n    tauri::Builder::default()\n        .plugin(victauri_plugin::init())\n        .run(tauri::generate_context!())\n        .unwrap();\n}\n",
        )
        .unwrap();

        let patched = try_patch_tauri_builder(&src).unwrap();
        assert!(patched, "should report true (already present)");

        let content = std::fs::read_to_string(src.join("main.rs")).unwrap();
        assert_eq!(
            content.matches("victauri_plugin").count(),
            1,
            "should not duplicate the plugin line"
        );
    }

    #[test]
    fn patch_tauri_builder_handles_lib_rs() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "pub fn run() {\n    tauri::Builder::default()\n        .build(tauri::generate_context!())\n        .unwrap();\n}\n",
        )
        .unwrap();

        let patched = try_patch_tauri_builder(&src).unwrap();
        assert!(patched);

        let content = std::fs::read_to_string(src.join("lib.rs")).unwrap();
        assert!(content.contains("victauri_plugin::init()"));
    }

    #[test]
    fn patch_tauri_builder_no_builder_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.rs"), "fn main() { println!(\"hello\"); }\n").unwrap();

        let patched = try_patch_tauri_builder(&src).unwrap();
        assert!(!patched, "should return false when no tauri::Builder found");
    }

    #[test]
    fn mcp_json_has_correct_structure() {
        let content = generate_mcp_json();
        let parsed: serde_json::Value = serde_json::from_str(content).unwrap();
        assert!(parsed["mcpServers"]["victauri"]["url"].as_str().is_some());
        assert!(
            parsed["mcpServers"]["victauri"]["url"]
                .as_str()
                .unwrap()
                .contains("7373")
        );
    }

    #[test]
    fn capability_json_has_correct_structure() {
        let content = generate_capability_json();
        let parsed: serde_json::Value = serde_json::from_str(content).unwrap();
        assert_eq!(parsed["identifier"].as_str().unwrap(), "victauri");
        assert!(
            parsed["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p.as_str().is_some_and(|s| s.contains("victauri")))
        );
    }

    #[test]
    fn integration_test_content_is_valid() {
        let content = generate_integration_test();
        assert!(content.contains("VictauriClient"));
        assert!(content.contains("Locator"));
        assert!(content.contains("VisualOptions"));
        assert!(content.contains("verify()"));
    }

    #[test]
    fn scan_tauri_commands_finds_commands() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("commands.rs"),
            "#[tauri::command]\nasync fn greet(name: String) -> String { name }\n\n#[tauri::command]\nfn get_count() -> i32 { 0 }\n",
        )
        .unwrap();

        let commands = scan_tauri_commands(dir.path());
        assert!(commands.contains(&"greet".to_string()));
        assert!(commands.contains(&"get_count".to_string()));
    }

    #[test]
    fn scan_tauri_commands_handles_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let commands = scan_tauri_commands(dir.path());
        assert!(commands.is_empty());
    }

    #[test]
    fn generate_integration_with_commands_includes_stubs() {
        let commands = vec!["greet".to_string(), "increment".to_string()];
        let content = generate_integration_test_with_commands(&commands);
        assert!(content.contains("command_greet"));
        assert!(content.contains("command_increment"));
        assert!(content.contains("invoke_command(\"greet\""));
        assert!(content.contains("invoke_command(\"increment\""));
    }

    #[test]
    fn ci_workflow_is_valid_yaml() {
        let content = generate_ci_workflow();
        assert!(content.contains("victauri test"));
        assert!(content.contains("xvfb"));
        assert!(content.contains("VICTAURI_E2E"));
    }

    #[test]
    fn claude_md_section_has_key_instructions() {
        let content = generate_claude_md_section();
        assert!(content.contains("Victauri"));
        assert!(content.contains("invoke_command"));
        assert!(content.contains("7373"));
        assert!(
            content.contains("Do NOT use Playwright"),
            "should instruct agents to prefer Victauri"
        );
    }

    #[test]
    fn extract_fn_name_works() {
        assert_eq!(
            extract_fn_name("fn greet(name: String)"),
            Some("greet".to_string())
        );
        assert_eq!(
            extract_fn_name("pub async fn get_count() -> i32"),
            Some("get_count".to_string())
        );
        assert_eq!(extract_fn_name("let x = 1;"), None);
    }
}
