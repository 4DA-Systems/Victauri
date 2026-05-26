<p align="center">
  <img src="assets/logo.png" alt="Victauri" width="120">
</p>

<h1 align="center">Victauri</h1>

<p align="center">
  <strong>Full-stack testing for Tauri apps. Click a button in the frontend, verify the Rust handler ran, confirm the database row was written — from one test.</strong>
</p>

<p align="center">
  <a href="https://github.com/runyourempire/victauri/actions/workflows/ci.yml"><img src="https://github.com/runyourempire/victauri/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://crates.io/crates/victauri-plugin"><img src="https://img.shields.io/crates/v/victauri-plugin.svg" alt="crates.io"></a>
  <a href="https://docs.rs/victauri-plugin"><img src="https://docs.rs/victauri-plugin/badge.svg" alt="docs.rs"></a>
  <a href="https://opensource.org/licenses/Apache-2.0"><img src="https://img.shields.io/badge/License-Apache_2.0-blue.svg" alt="License: Apache-2.0"></a>
  <a href="https://doc.rust-lang.org/edition-guide/rust-2024/index.html"><img src="https://img.shields.io/badge/MSRV-1.88+-informational" alt="MSRV: 1.88+"></a>
</p>

---

Testing Tauri apps today means choosing between frontend mocks that lie about your backend, WebDriver setups that take a weekend, or paying for macOS support. Victauri embeds an [MCP](https://modelcontextprotocol.io) server directly inside your Tauri process — giving test suites and AI agents simultaneous access to the DOM, IPC layer, Rust backend state, and native windows. No WebDriver. No browser dependency. **Works on macOS, Windows, and Linux.**

> **Tested against 5 real-world Tauri apps** (Kanri, En Croissant, Surrealist, Duckling, Lettura) — **96.9% pass rate** across 895 tests with zero Victauri bugs and zero code changes.

## What You Get

- **Full-stack verification** — click a button, verify the IPC call, query the database to confirm the write, check the UI updated
- **Direct backend access** — query SQLite databases, browse app files, read config, inspect process memory — no webview proxy
- **Ghost command detection** — find frontend calls with no backend handler, and backend commands the frontend never calls
- **Cross-boundary state checking** — compare DOM state against Rust backend state, catch drift automatically
- **Time-travel recording** — record interactions, checkpoint state, replay sequences, generate test files
- **Zero production cost** — compiles away entirely in release builds via `#[cfg(debug_assertions)]`
- **AI agent compatible** — speaks MCP protocol, works with Claude Code, Cursor, Windsurf, and any MCP client

## Quick Start

### Install the CLI

```bash
cargo install victauri-cli
```

### Set up your project

From your Tauri project root:

```bash
victauri init
```

This will:
- Add `victauri-plugin` and `victauri-test` to your `Cargo.toml`
- Create starter smoke tests in your `tests/` directory
- Print the next steps to wire the plugin

### Wire the plugin

Add one line to your Tauri builder:

```rust
// src-tauri/src/main.rs (or lib.rs)
tauri::Builder::default()
    .plugin(victauri_plugin::init())
    .invoke_handler(tauri::generate_handler![/* your commands */])
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
```

In release builds, `init()` returns a no-op plugin — zero overhead, no feature flags needed.

### Run tests

Start your app, then run the smoke suite:

```bash
pnpm tauri dev                                   # start your app
VICTAURI_E2E=1 cargo test --test smoke           # run tests
```

Or use the CLI for instant validation:

```bash
victauri test                                    # 11 built-in smoke checks
victauri check                                   # server health + IPC diagnostics
```

### Connect an AI agent

Add `.mcp.json` to your project root (created automatically by `victauri init`):

```json
{
  "mcpServers": {
    "my-app": {
      "url": "http://127.0.0.1:7373/mcp"
    }
  }
}
```

Works with **Claude Code**, **Cursor**, **Windsurf**, and any MCP client. Your agent gets full-stack access: DOM snapshots, IPC monitoring, command invocation, screenshot capture, accessibility auditing, and more.

**With Claude Code**, start your app and Claude can immediately:
- Inspect the DOM tree and click/type/fill any element
- Invoke any `#[tauri::command]` and verify the response
- Read app config, list files, query SQLite databases
- Take screenshots and audit accessibility
- Record interactions and replay them as tests

---

## Writing Tests

The `e2e_test!` macro handles server detection and auto-connect:

```rust
use victauri_test::{e2e_test, VictauriClient};

e2e_test!(greet_flow, |client| async move {
    client.fill_by_id("name-input", "World").await.unwrap();
    client.click_by_id("greet-btn").await.unwrap();
    client.expect_text("Hello, World!").await.unwrap();
});
```

### Locator API

For complex queries, composable locators with auto-waiting expectations:

```rust
use victauri_test::prelude::*;

e2e_test!(settings_flow, |client| async move {
    let save = Locator::role("button").and_text("Save");
    let email = Locator::label("Email address");

    email.fill(&mut client, "user@example.com").await.unwrap();
    save.click(&mut client).await.unwrap();

    Locator::test_id("toast-message")
        .expect(&mut client)
        .to_contain_text("Settings saved")
        .await
        .unwrap();
});
```

Locators support `role`, `text`, `test_id`, `css`, `label`, `placeholder`, `alt_text`, and `title` strategies with chainable refinement (`.and_text()`, `.nth()`, `.and_tag()`). See the [Testing Guide](docs/src/testing.md) for the full Locator API reference.

### Direct Client Methods

| Method | What it does |
|---|---|
| `click_by_text("Submit")` | Find element by visible text, click it |
| `click_by_id("save-btn")` | Find element by HTML id, click it |
| `fill_by_id("email", "a@b.com")` | Find input by id, fill value |
| `type_by_id("search", "query")` | Find input by id, type char-by-char |
| `select_by_id("theme", "dark")` | Find select by id, choose option |
| `expect_text("Success!")` | Poll until text appears (5s timeout) |
| `expect_no_text("Error")` | Poll until text disappears (3s timeout) |
| `text_by_id("counter")` | Get text content of element by id |

---

## Full-Stack Verification

This is what sets Victauri apart — verifying that frontend actions actually trigger the right backend logic.

### UI to IPC to Database

```rust
// Click "Save" in the UI
client.click_by_id("save-btn").await?;

// Verify the IPC command was called
let log = client.get_ipc_log(None).await?;
assert_ipc_called(&log, "save_settings");

// Verify the database was actually written
let result = client.query_db(
    "SELECT value FROM settings WHERE key = 'theme'",
    None, None,
).await?;
assert_eq!(result["rows"][0]["value"], "dark");
```

### Fluent Verification

Check multiple conditions at once — DOM, IPC, accessibility, errors — with a single report:

```rust
client.verify()
    .has_text("Settings saved")
    .ipc_was_called("save_settings")
    .no_console_errors()
    .no_ghost_commands()
    .ipc_healthy()
    .coverage_above(80.0)
    .run()
    .await?
    .assert_all_passed();
```

### Ghost Command Detection

Find orphaned commands — called in the frontend but missing from the backend:

```rust
let ghosts = client.detect_ghost_commands().await?;
assert!(ghosts["ghost_commands"].as_array().unwrap().is_empty(),
    "Found ghost commands: {ghosts}");
```

See the [Testing Guide](docs/src/testing.md) for IPC checkpoints, visual regression testing, IPC coverage, accessibility auditing, performance monitoring, time-travel recording, CI integration, and more.

---

## MCP Tools

31 tools across three layers — backend, IPC, and webview:

### Backend tools (direct Rust access, no webview needed)

| Tool | What it does |
|---|---|
| `app_info` | App config, directory paths, env vars, discovered databases, process info |
| `list_app_dir` | Browse files in app data/config/log/local_data directories |
| `read_app_file` | Read files from app backend directories (UTF-8 or base64) |
| `query_db` | Read-only SQLite queries with auto-discovery |
| `invoke_command` | Call any Tauri command directly through IPC |
| `get_memory_stats` | Real-time OS process memory (working set, page faults) |

### IPC tools

| Tool | What it does |
|---|---|
| `get_registry` | List all `#[inspectable]` command schemas |
| `detect_ghost_commands` | Find orphaned frontend IPC calls with no backend handler |
| `check_ipc_integrity` | Detect stuck/stale/errored IPC calls |
| `verify_state` | Compare frontend DOM against backend state |
| `resolve_command` | Natural language to matching Tauri command |

### Webview tools

| Tool | What it does |
|---|---|
| `eval_js` | Execute JavaScript in the webview |
| `dom_snapshot` | Full accessibility tree with ref handles |
| `find_elements` | Search by text, role, test ID, CSS, label, placeholder, alt, title |
| `screenshot` | Platform-native window capture (no Chromium) |
| `wait_for` | Poll for conditions: text, selector, IPC settle |
| `assert_semantic` | Evaluate JS + assert against expected value |

### Compound tools (multiple actions per tool)

| Tool | Actions |
|---|---|
| **`interact`** | `click`, `double_click`, `hover`, `focus`, `scroll`, `select` |
| **`input`** | `fill`, `type_text`, `press_key` (keyboard combos supported) |
| **`window`** | `get_state`, `list`, `manage`, `resize`, `move`, `set_title` |
| **`storage`** | `get`, `set`, `delete`, `cookies` |
| **`navigate`** | `go_to`, `back`, `history`, `dialogs` |
| **`recording`** | `start`, `stop`, `checkpoint`, `events`, `export`, `import` |
| **`inspect`** | `styles`, `bounds`, `highlight`, `audit_accessibility`, `get_performance` |
| **`logs`** | `console`, `network`, `ipc`, `navigation`, `dialogs`, `events`, `slow_ipc` |
| **`css`** | `inject`, `remove` |
| **`introspect`** | `command_timings`, `coverage`, `contract_record`, `contract_check`, `startup_timing`, `capabilities`, `db_health`, `plugin_state`, `processes`, `plugin_tasks`, `event_bus` |
| **`fault`** | `inject` (delay/error/drop/corrupt), `list`, `clear`, `clear_all` |
| **`explain`** | `summary`, `last_action`, `diff` |
| `get_plugin_info` | Plugin config: port, tools, privacy, version |
| `get_diagnostics` | Shadow DOM, service workers, iframes, large DOM detection |

All tools are also available via REST at `POST /api/tools/{name}` — no MCP client needed. See the [Tools Reference](docs/src/tools-reference.md).

---

## How It Works

```
AI Agent / cargo test / curl
        |
        v
  HTTP on :7373
  ├── /mcp          (MCP protocol — for AI agents)
  ├── /api/tools    (REST API — for scripts and CI)
  └── /health       (health check — for monitoring)
        |
        v
  Victauri Plugin  (inside Tauri process)
     |       |       |
     v       v       v
  WebView  IPC    Backend
  - DOM    - log   - app config
  - click  - args  - file system
  - eval   - cmds  - SQLite DBs
  - a11y   - ghost - memory
  - perf   - verify- env vars
```

Victauri runs **inside** the Tauri process — same thread pool, same memory space. This isn't an implementation detail; it changes what's possible:

| | Embedded (Victauri) | External process |
|---|---|---|
| **Tool response** | <1ms (function call) | 5-50ms (IPC + serialization) |
| **State accuracy** | Zero drift (reads live state) | Stale (snapshot + transfer) |
| **Backend access** | Full (AppHandle, DB, state) | Limited (webview only) |
| **Setup** | One line in Cargo.toml | Separate process + config |
| **Release build** | Compiles away entirely | Must be disabled manually |

**Port selection:** Victauri tries port 7373 first, then falls back through 7374-7383 if taken. The actual port is written to a temp directory for automatic client discovery.

---

## Architecture

```
victauri/
├── crates/
│   ├── victauri-plugin/     # Tauri plugin + MCP server + JS bridge (the main crate)
│   ├── victauri-browser/    # Native host: MCP for any website via browser extension
│   ├── victauri-core/       # Shared types (events, registry, snapshots, verification)
│   ├── victauri-macros/     # #[inspectable] proc macro for command schemas
│   ├── victauri-test/       # Test client + Locator API + assertion helpers
│   ├── victauri-cli/        # CLI: init, check, test, record, watch, coverage
│   └── victauri-watchdog/   # Health-check sidecar for crash recovery
├── extensions/
│   ├── chrome/              # Chrome/Edge/Brave extension (MV3) + 163 vitest tests
│   ├── firefox/             # Firefox extension (MV3)
│   └── npm/                 # victauri-browser npm package
├── docs/                    # mdbook documentation site
└── examples/
    └── demo-app/            # Multi-window Tauri app with 19 instrumented commands
```

| Crate | Purpose | Tauri dependency? |
|---|---|---|
| `victauri-plugin` | Embed in your app — MCP server + bridge | Yes |
| `victauri-browser` | Browser extension native host — any website | No |
| `victauri-test` | Use in your tests — client + assertions | No |
| `victauri-cli` | Install globally — scaffold + diagnose | No |
| `victauri-macros` | Use on commands — `#[inspectable]` | No |
| `victauri-core` | Shared types (usually not used directly) | No |
| `victauri-watchdog` | Run as sidecar for crash recovery | No |

---

## Browser Extension

Victauri isn't limited to Tauri apps. The browser extension + native host brings the same MCP inspection to **any website**:

```bash
cargo install victauri-browser
victauri-browser-host install    # Registers native messaging host
```

Load `extensions/chrome/` (or `extensions/firefox/`) as an unpacked extension. Connect any MCP client to `http://127.0.0.1:7474/mcp`. 20 MCP tools available. Works with Chrome, Edge, Brave, Arc, and Firefox.

See the [Chrome Extension Guide](docs/src/chrome-extension.md).

---

## Security

Victauri is designed for development, not production:

- **Debug-only**: compiles away in release builds (`#[cfg(debug_assertions)]`)
- **Localhost-only**: binds to 127.0.0.1, DNS rebinding protection
- **Optional auth**: Bearer token authentication for shared/CI environments
- **Rate limited**: 1000 req/sec, token-bucket algorithm
- **Privacy profiles**: `Observe` (read-only), `Test` (interactions), `FullControl` (everything)
- **Output redaction**: auto-scrub API keys, tokens, emails from tool responses

See the [Security Guide](docs/src/security.md) for threat model, privacy configuration, and command filtering.

---

## CI Integration

### GitHub Actions

Use the built-in composite action to run Victauri smoke tests in CI:

```yaml
- name: Build my app
  run: cargo build -p my-app

- name: Start app under xvfb
  run: |
    xvfb-run -a ./target/debug/my-app &
    sleep 3

- uses: runyourempire/victauri/.github/actions/victauri-test@main
  with:
    max-load-ms: 10000
    max-heap-mb: 512
```

The action installs `victauri-cli`, waits for the server to become healthy, and runs all 11 smoke checks. Available inputs:

| Input | Default | Description |
|---|---|---|
| `port` | `7373` | Victauri server port |
| `max-load-ms` | `10000` | Maximum DOM complete time (ms) |
| `max-heap-mb` | `512` | Maximum JS heap usage (MB) |
| `junit-path` | | Path to write JUnit XML report |
| `coverage` | `false` | Run IPC coverage report after tests |
| `coverage-threshold` | | Minimum coverage % (fails if below) |
| `health-timeout` | `30` | Seconds to wait for server health |

Or use `victauri init` to generate a complete CI workflow file for your project:

```bash
victauri init    # generates .github/workflows/victauri.yml
```

---

## Documentation

- [**Getting Started**](docs/src/getting-started.md) — Setup, capabilities, first connection
- [**Testing Guide**](docs/src/testing.md) — Locator API, IPC verification, visual regression, CI integration
- [**Tools Reference**](docs/src/tools-reference.md) — All 31 tools with parameters and examples
- [**Architecture**](docs/src/architecture.md) — Embedded design, JS bridge, dual protocol
- [**Configuration**](docs/src/configuration.md) — Port, auth, privacy, capacity tuning
- [**Security**](docs/src/security.md) — Threat model, privacy profiles, redaction
- [**Chrome Extension**](docs/src/chrome-extension.md) — Browser MCP for any website
- [**FAQ**](docs/src/faq.md) — Common questions and troubleshooting
- [**VS Code Extension**](editors/vscode/) — Live inspection from your editor
- [**Demo App**](examples/demo-app/) — Reference app with 19 instrumented commands
- [**Agent Session**](examples/agent-session.md) — Real AI agent transcript
- [**Migration Guide**](MIGRATION.md) — Upgrading between versions
- [**Contributing**](CONTRIBUTING.md) — How to contribute
- [**Changelog**](CHANGELOG.md) — Release history

---

## What It Doesn't Do

- **No production use** — debug builds only, by design
- **No remote access** — localhost only, no port forwarding
- **No iframe support** — single-frame webviews only (Tauri standard)
- **Pre-1.0** — API may change (semver-checked in CI)

---

## Development

```bash
cargo build --workspace                               # Build all crates
cargo test --workspace                                # Run all tests
cargo bench -p victauri-core                          # Criterion benchmarks (16)
cargo clippy --workspace --all-targets                # Lint (20 enforced lints)
cargo fmt --all -- --check                            # Format
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps  # Docs (zero warnings)
```

**Lint policy:** 20 clippy lints (pedantic + nursery) enforced at `deny` level — see `[workspace.lints.clippy]` in `Cargo.toml`.

## License

Apache-2.0 — [LICENSE](LICENSE)

Built and maintained by [4DA Systems](https://4da.ai).
