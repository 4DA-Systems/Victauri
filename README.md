# Victauri

**Full-stack testing for Tauri apps. Click a button in the frontend, verify the Rust handler ran, confirm the database row was written — from one test.**

[![CI](https://github.com/runyourempire/victauri/actions/workflows/ci.yml/badge.svg)](https://github.com/runyourempire/victauri/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/victauri-plugin.svg)](https://crates.io/crates/victauri-plugin)
[![docs.rs](https://docs.rs/victauri-plugin/badge.svg)](https://docs.rs/victauri-plugin)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

---

Testing Tauri apps today means choosing between frontend mocks that lie about your backend, WebDriver setups that take a weekend, or paying for macOS support. Victauri embeds an [MCP](https://modelcontextprotocol.io) server directly inside your Tauri process — giving test suites and AI agents simultaneous access to the DOM, IPC layer, Rust backend state, and native windows. No WebDriver binary. No browser dependency. **Works on macOS, Windows, and Linux for free.**

## What makes this different

Every other Tauri testing tool stops at the glass. They can click buttons and read text, but they can't tell you whether the Rust command handler actually executed, what arguments it received, or whether the result made it back to the frontend intact.

Victauri crosses the boundary:

```rust
// Click a button by visible text — no selectors, no ref handles
client.click_by_text("Save").await?;

// Verify the Rust command ran with correct args
let ipc = client.get_ipc_log(Some(1)).await?;
assert_eq!(ipc[0]["command"], "save_settings");
assert_eq!(ipc[0]["args"]["theme"], "dark");

// Verify frontend state matches backend
let result = client.verify_state(
    "document.querySelector('.theme-label').textContent",
    json!({"theme": "dark"})
).await?;
assert!(result["divergences"].as_array().unwrap().is_empty());
```

| | Playwright | WebdriverIO + tauri-driver | tauri-plugin-mcp | mcp-server-tauri | **Victauri** |
|---|---|---|---|---|---|
| DOM interaction | Yes | Yes | Yes | Yes | **Yes** |
| macOS support | No (no CDP) | No (no WKWebView driver) | Yes | Yes | **Yes** |
| Backend state access | No | No | No | No | **Yes** |
| IPC call verification (args + result) | No | No | No | Partial | **Yes** |
| Ghost command detection | No | No | No | No | **Yes** |
| Cross-boundary verification | No | No | No | No | **Yes** |
| `#[inspectable]` command schemas | No | No | No | No | **Yes** |
| Time-travel recording | No | No | No | No | **Yes** |
| Visual regression testing | Plugin | Plugin | No | No | **Yes** |
| IPC coverage tracking | No | No | No | No | **Yes** |
| Record → generate tests | Codegen | No | No | No | **Yes** |
| Zero-config setup | No | No | Near | Near | **Yes** |
| Works with `cargo test` | N/A | Limited | No | No | **Yes** |

## Quick Start

**1. Add the crates:**

```toml
# Cargo.toml
[dev-dependencies]
victauri-plugin = "0.1"
victauri-test = "0.1"
```

**2. Wire it up** (two lines in your app):

```rust
// src-tauri/src/main.rs
tauri::Builder::default()
    .plugin(
        victauri_plugin::VictauriBuilder::new()
            .commands(&[
                greet__schema(),
                save_settings__schema(),
            ])
            .build()
            .unwrap(),
    )
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
```

Or minimal — `init()` works if you don't need the command registry:

```rust
tauri::Builder::default()
    .plugin(victauri_plugin::init())
    // ...
```

In release builds, both return a no-op plugin — zero overhead, no feature flags needed.

**3. Connect your agent or test runner:**

For AI agents (Claude Code, Cursor, Windsurf) — add `.mcp.json` to your project:

```json
{
  "mcpServers": {
    "my-app": {
      "url": "http://127.0.0.1:7373/mcp"
    }
  }
}
```

For `cargo test` — use `TestApp` for managed lifecycle and Playwright-style assertions:

```rust
use victauri_test::TestApp;

#[tokio::test]
async fn greet_flow() {
    let app = TestApp::spawn("cargo run -p my-app").await.unwrap();
    let mut client = app.client().await.unwrap();

    client.fill_by_id("name-input", "World").await.unwrap();
    client.click_by_id("greet-btn").await.unwrap();
    client.expect_text("Hello, World!").await.unwrap();
}

#[tokio::test]
async fn todo_crud() {
    let app = TestApp::spawn("cargo run -p my-app").await.unwrap();
    let mut client = app.client().await.unwrap();

    client.fill_by_id("todo-input", "Ship it").await.unwrap();
    client.click_by_text("Add").await.unwrap();
    client.expect_text("Ship it").await.unwrap();

    // Verify backend state matches frontend
    let result = client.verify_state(
        "document.querySelector('.todo-item').textContent",
        json!({"title": "Ship it"})
    ).await.unwrap();
    assert!(result["divergences"].as_array().unwrap().is_empty());
}
```

Or connect to an already-running app for lower-level control:

```rust
use victauri_test::VictauriClient;

#[tokio::test]
async fn cross_boundary_verification() {
    let mut client = VictauriClient::discover().await.unwrap();

    // Invoke backend command directly
    client.invoke_command("save_settings", Some(json!({"theme": "dark"}))).await.unwrap();

    // Verify the UI updated
    client.expect_text("Dark mode").await.unwrap();

    // Check no ghost commands exist
    let ghosts = client.detect_ghost_commands().await.unwrap();
    assert!(ghosts["ghost_commands"].as_array().unwrap().is_empty());
}
```

## MCP Tools

Victauri exposes 23 MCP tools — 9 compound tools and 14 standalone:

| Tool | What it does |
|---|---|
| **`interact`** | Click, double-click, hover, focus, scroll, select — with auto-wait for actionability |
| **`input`** | Fill inputs, type character-by-character, press keyboard keys |
| **`window`** | Get state, list windows, manage (minimize/maximize/etc), resize, move, set title |
| **`storage`** | Read/write localStorage, sessionStorage, cookies |
| **`navigate`** | Go to URL, go back, get history, configure dialog auto-responses |
| **`recording`** | Start/stop sessions, create checkpoints, get events, export/import for replay |
| **`inspect`** | Computed CSS, bounding boxes, element highlighting, accessibility audit, performance metrics |
| **`logs`** | Console, network, IPC, navigation, dialog logs — filtered by time and content |
| **`css`** | Inject/remove debug CSS |
| `eval_js` | Execute JavaScript in the webview (async supported) |
| `dom_snapshot` | Full accessibility tree with ref handles for interaction |
| `find_elements` | Search for elements by text, role, test ID, or CSS selector without a full snapshot |
| `invoke_command` | Call any registered Tauri command through real IPC |
| `screenshot` | Platform-native window capture (no Chromium dependency) |
| `verify_state` | Compare frontend DOM state against backend state — find divergences |
| `detect_ghost_commands` | Find frontend IPC calls with no backend handler (and vice versa) |
| `check_ipc_integrity` | Detect stuck, stale, or errored IPC calls |
| `wait_for` | Poll for conditions: text appears, selector matches, IPC settles |
| `assert_semantic` | Evaluate JS expression and assert against expected value |
| `resolve_command` | Natural language → matching Tauri command |
| `get_registry` | List all commands with schemas from `#[inspectable]` |
| `get_memory_stats` | Real-time process memory statistics from the OS |
| `get_plugin_info` | Victauri config: port, enabled tools, privacy settings, version |

## Test API

The `victauri-test` crate provides Playwright-style convenience methods that handle DOM snapshots and ref resolution internally:

| Method | What it does |
|---|---|
| `click_by_text("Submit")` | Find element by visible text → click |
| `click_by_id("save-btn")` | Find element by HTML id → click |
| `fill_by_id("email", "a@b.com")` | Find input by id → fill value |
| `type_by_id("search", "query")` | Find input by id → type char-by-char |
| `select_by_id("theme", "dark")` | Find select by id → choose option |
| `expect_text("Success!")` | Poll until text appears (5s timeout) |
| `expect_no_text("Error")` | Poll until text disappears (3s timeout) |
| `text_by_id("counter")` | Get text content of element by id |
| `screenshot_visual("name", &opts)` | Visual regression — compare against stored baseline |
| `verify().coverage_above(80.0)` | Assert IPC command coverage meets threshold |

Plus lower-level methods for direct ref-handle interaction, IPC inspection, recording, accessibility audits, and performance profiling.

### IPC Assertions

Verify your backend commands actually ran — with the right arguments:

```rust
use victauri_test::{assert_ipc_called, assert_ipc_called_with, assert_ipc_not_called};

// After user interaction...
let log = client.get_ipc_log(None).await?;

assert_ipc_called(&log, "save_settings");
assert_ipc_called_with(&log, "save_settings", &json!({"theme": "dark"}));
assert_ipc_not_called(&log, "delete_account");
```

Or use checkpoints to assert only on calls made during a specific action:

```rust
let checkpoint = client.ipc_checkpoint().await?;

client.click_by_id("save-btn").await?;

let calls = client.ipc_calls_since(checkpoint).await?;
assert_eq!(calls.len(), 1);
assert_eq!(calls[0]["command"], "save_settings");
```

### Fluent Verification

Check everything at once — DOM, IPC, network, errors — with a single report:

```rust
let report = client.verify()
    .has_text("Settings saved")
    .has_no_text("Error")
    .ipc_was_called("save_settings")
    .ipc_was_called_with("save_settings", json!({"theme": "dark"}))
    .ipc_was_not_called("delete_account")
    .no_console_errors()
    .ipc_healthy()
    .run()
    .await?;

report.assert_all_passed();
// Or inspect individual results:
// report.failures() → Vec<&CheckResult>
```

### Zero-Boilerplate Tests

```rust
use victauri_test::{e2e_test, VictauriClient};

e2e_test!(greet_flow, |client| async move {
    client.fill_by_id("name-input", "World").await.unwrap();
    client.click_by_id("greet-btn").await.unwrap();
    client.expect_text("Hello, World!").await.unwrap();
});
```

The `e2e_test!` macro handles skip-when-no-server and auto-connect.

### Visual Regression

Compare screenshots against stored baselines — pixel-level diffing with configurable tolerance:

```rust
use victauri_test::visual::VisualOptions;

let opts = VisualOptions {
    snapshot_dir: "tests/snapshots".into(),
    channel_tolerance: 2,   // per-channel tolerance (0-255)
    threshold_percent: 0.5, // max % of pixels that may differ
    ..Default::default()
};

let diff = client.screenshot_visual("dashboard", &opts).await?;
assert!(diff.is_match, "visual regression: {:.2}% pixels differ", diff.diff_percentage);
```

On first run, the screenshot is saved as the baseline. Subsequent runs compare against it and generate a red-overlay diff image when mismatched.

### IPC Coverage

Track which registered commands your tests actually exercise:

```rust
use victauri_test::coverage::coverage_report;

let report = coverage_report(&mut client).await?;
assert!(report.meets_threshold(80.0), "{}", report.to_summary());
```

Or inline with the fluent builder:

```rust
client.verify()
    .has_text("Welcome")
    .coverage_above(80.0)
    .run().await?.assert_all_passed();
```

## CLI

The `victauri` CLI scaffolds, checks, records, and measures:

```bash
victauri init                          # Scaffold test directory with starter tests
victauri check                         # Connect to running app, report health
victauri check --junit report.xml      # Same, with JUnit XML output
victauri record --output tests/flow.rs # Record interactions → generate Rust test
victauri coverage --threshold 80       # Report IPC coverage, fail if below 80%
victauri watch                         # Re-run tests on file changes
```

## Instrument Your Commands

```rust
use victauri_macros::inspectable;

#[tauri::command]
#[inspectable(
    description = "Save user preferences",
    intent = "persist settings",
    category = "settings",
    example = "save the user's theme preference"
)]
async fn save_settings(settings: Settings) -> Result<(), AppError> {
    // your code
}
```

`#[inspectable]` generates a command schema at compile time — zero runtime cost. Commands become discoverable through `get_registry` and natural language via `resolve_command`.

## How It Works

```
AI Agent / cargo test
        |
        v
  HTTP on :7373  (MCP protocol)
        |
        v
  Victauri Plugin  (inside Tauri process)
     |       |       |
     v       v       v
  WebView  IPC    Backend
  - DOM    - log   - state
  - click  - args  - memory
  - eval   - result - registry
```

The MCP server runs **inside** the Tauri process — not as a sidecar. Direct `AppHandle` access means sub-millisecond response times and zero state drift between what the tool sees and what the app does.

## Security & Privacy

Victauri is designed for development, not production:

- **Debug-only**: entire plugin compiles away in release builds (`#[cfg(debug_assertions)]`)
- **Localhost-only**: no remote access, DNS rebinding protection
- **Auth tokens**: auto-generated or configurable via `VICTAURI_AUTH_TOKEN`
- **Privacy controls**: command allowlists/blocklists, tool disabling, output redaction (API keys, JWTs, emails)
- **Strict mode**: one call to disable all mutating tools

```rust
VictauriBuilder::new()
    .strict_privacy_mode()   // disable eval_js, fill, type_text, navigate, etc.
    .auth_token("my-token")
    .build()
```

## Architecture

```
victauri/
├── crates/
│   ├── victauri-core/       # Shared types (events, registry, snapshots, verification)
│   ├── victauri-macros/     # #[inspectable] proc macro
│   ├── victauri-plugin/     # Tauri plugin + MCP server + JS bridge
│   ├── victauri-test/       # Typed HTTP client + assertion helpers
│   └── victauri-watchdog/   # Health-check sidecar
└── examples/
    └── demo-app/            # Minimal Tauri app with 12 instrumented commands
```

## CI Integration

Victauri tests run in CI without special infrastructure. Start your app, run the tests:

```yaml
# .github/workflows/test.yml
- name: Build app
  run: cargo build -p my-app

- name: Start app (Linux needs xvfb for headless)
  run: xvfb-run --auto-servernum cargo run -p my-app &

- name: Run Victauri tests
  run: cargo test -p my-app --test integration -- --test-threads=1
  env:
    VICTAURI_E2E: "1"
    VICTAURI_PORT: "7374"
```

With `TestApp::spawn`, the lifecycle is managed automatically — no background process needed:

```yaml
- name: Run tests
  run: cargo test -p my-app --test integration
```

Linux CI requires a virtual display (`xvfb-run`) since Tauri/WebView needs a display server.
`TestApp::spawn` detects missing display and gives a clear error message.

## What It Doesn't Do

- **No production use** — debug builds only, by design
- **No remote access** — localhost, no port forwarding
- **No iframe support** — single-frame webviews only (Tauri standard)
- **Pre-1.0** — API may change. Semver-checked in CI.

## Development

```bash
cargo build --workspace                               # Build all crates
cargo test --workspace                                # Run all 838 tests
cargo bench -p victauri-core                          # Criterion benchmarks (16)
cargo clippy --workspace --all-targets                # Lint (20 enforced lints)
cargo fmt --all -- --check                            # Format
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps  # Docs (zero warnings)
```

**Lint policy:** 20 clippy lints (pedantic + nursery) are enforced at `deny` level workspace-wide — see `[workspace.lints.clippy]` in `Cargo.toml`. PRs that introduce warnings won't compile.

## License

Apache-2.0 — [LICENSE](LICENSE)

Built and maintained by [4DA Systems](https://4da.ai).
