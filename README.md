# Victauri

**Full-stack testing for Tauri apps. Click a button in the frontend, verify the Rust handler ran, confirm the database row was written — from one test.**

[![CI](https://github.com/runyourempire/victauri/actions/workflows/ci.yml/badge.svg)](https://github.com/runyourempire/victauri/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/victauri-plugin.svg)](https://crates.io/crates/victauri-plugin)
[![docs.rs](https://docs.rs/victauri-plugin/badge.svg)](https://docs.rs/victauri-plugin)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![MSRV: 1.88+](https://img.shields.io/badge/MSRV-1.88+-informational)](https://doc.rust-lang.org/edition-guide/rust-2024/index.html)

---

Testing Tauri apps today means choosing between frontend mocks that lie about your backend, WebDriver setups that take a weekend, or paying for macOS support. Victauri embeds an [MCP](https://modelcontextprotocol.io) server directly inside your Tauri process — giving test suites and AI agents simultaneous access to the DOM, IPC layer, Rust backend state, and native windows. No WebDriver binary. No browser dependency. **Works on macOS, Windows, and Linux.**

## What You Get

Victauri gives you capabilities that no other Tauri testing tool provides:

- **Full-stack verification** — click a button, confirm the IPC call went through, verify the Rust handler ran with correct arguments, check the UI updated
- **Ghost command detection** — find frontend calls with no backend handler, and backend commands no frontend ever calls
- **Cross-boundary state checking** — compare what the DOM says against what the Rust backend knows, catch state drift automatically
- **IPC coverage tracking** — know exactly which Tauri commands your tests exercise and which have zero coverage
- **Time-travel recording** — record interactions, checkpoint state, replay sequences, generate test files
- **Visual regression testing** — pixel-level screenshot comparison with configurable tolerance
- **Accessibility auditing** — WCAG checks for alt text, labels, contrast, ARIA roles, heading hierarchy
- **Performance profiling** — navigation timing, JS heap usage, resource loading, long task detection
- **Zero production cost** — entire plugin compiles away in release builds via `#[cfg(debug_assertions)]`
- **Cross-platform** — macOS, Windows, and Linux without WebDriver or Chromium dependencies
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

Add `.mcp.json` to your project root (works with Claude Code, Cursor, Windsurf):

```json
{
  "mcpServers": {
    "my-app": {
      "url": "http://127.0.0.1:7373/mcp"
    }
  }
}
```

Your AI agent now has full-stack access to your running Tauri app — DOM inspection, IPC monitoring, command invocation, screenshot capture, and more.

---

## Writing Tests

### Direct Client Methods

The simplest way to interact with your app — find elements by text or ID, no selectors needed:

```rust
use victauri_test::prelude::*;

#[tokio::test]
async fn greet_flow() {
    if !is_e2e() { return; }
    let mut client = VictauriClient::discover().await.unwrap();

    client.fill_by_id("name-input", "World").await.unwrap();
    client.click_by_id("greet-btn").await.unwrap();
    client.expect_text("Hello, World!").await.unwrap();
}
```

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

### Locator API

For complex queries, Victauri provides composable locators with auto-waiting expectations:

```rust
use victauri_test::prelude::*;

#[tokio::test]
async fn settings_flow() {
    if !is_e2e() { return; }
    let mut client = VictauriClient::discover().await.unwrap();

    // Find by role, refine by text
    let save_btn = Locator::role("button").and_text("Save");
    let email = Locator::label("Email address");
    let toast = Locator::test_id("toast-message");

    // Interact
    email.fill(&mut client, "user@example.com").await.unwrap();
    save_btn.click(&mut client).await.unwrap();

    // Auto-wait for result
    toast.expect(&mut client)
        .to_contain_text("Settings saved")
        .await
        .unwrap();

    // Verify it disappears
    toast.expect(&mut client)
        .timeout_ms(10_000)
        .not()
        .to_be_visible()
        .await
        .unwrap();
}
```

**Factory methods** — create locators by different strategies:

| Factory | Example | Finds by |
|---|---|---|
| `Locator::role("button")` | ARIA role | `role="button"` |
| `Locator::text("Submit")` | Visible text (substring) | `textContent` |
| `Locator::text_exact("OK")` | Visible text (exact match) | `textContent` |
| `Locator::test_id("login-btn")` | Test ID attribute | `data-testid` |
| `Locator::css(".nav > a")` | CSS selector | CSS query |
| `Locator::label("Email")` | Associated label text | `<label>` + `for` |
| `Locator::placeholder("Search...")` | Placeholder attribute | `placeholder` |
| `Locator::alt_text("Logo")` | Alt text (images) | `alt` |
| `Locator::title("Close")` | Title attribute | `title` |

**Refinement** — narrow results with chained filters:

```rust
// Button with role "button" AND text containing "Save"
let save = Locator::role("button").and_text("Save");

// Third item in a list
let third = Locator::role("listitem").nth(2);

// Input with specific tag
let textarea = Locator::label("Description").and_tag("textarea");
```

**Actions** — interact with resolved elements:

```rust
locator.click(&mut client).await?;
locator.double_click(&mut client).await?;
locator.fill(&mut client, "value").await?;
locator.type_text(&mut client, "typed").await?;
locator.press_key(&mut client, "Enter").await?;
locator.press_key(&mut client, "Control+a").await?;  // keyboard combos
locator.hover(&mut client).await?;
locator.focus(&mut client).await?;
locator.scroll_into_view(&mut client).await?;
locator.select_option(&mut client, &["dark"]).await?;
locator.check(&mut client).await?;                    // checkbox
locator.uncheck(&mut client).await?;
```

**Queries** — read element state:

```rust
let text = locator.text_content(&mut client).await?;
let value = locator.input_value(&mut client).await?;
let visible = locator.is_visible(&mut client).await?;
let enabled = locator.is_enabled(&mut client).await?;
let checked = locator.is_checked(&mut client).await?;
let focused = locator.is_focused(&mut client).await?;
let count = locator.count(&mut client).await?;
let bounds = locator.bounding_box(&mut client).await?;
let attr = locator.get_attribute(&mut client, "href").await?;
let all = locator.all(&mut client).await?;              // all matches
let texts = locator.all_text_contents(&mut client).await?;
```

**Expectations** — auto-waiting assertions with configurable timeout:

```rust
// Wait up to 5s (default) for element to become visible
locator.expect(&mut client).to_be_visible().await?;

// Custom timeout and polling
locator.expect(&mut client)
    .timeout_ms(10_000)
    .poll_ms(100)
    .to_have_text("Complete")
    .await?;

// Negation — wait until condition is NOT true
locator.expect(&mut client).not().to_be_visible().await?;
```

| Expectation | Waits until |
|---|---|
| `.to_be_visible()` | Element is visible |
| `.to_be_hidden()` | Element is hidden |
| `.to_be_enabled()` | Element is enabled |
| `.to_be_disabled()` | Element is disabled |
| `.to_be_focused()` | Element has focus |
| `.to_have_text("exact")` | Text content equals value |
| `.to_contain_text("partial")` | Text content contains value |
| `.to_have_value("input-val")` | Input value equals value |
| `.to_have_attribute("href", "/home")` | Attribute equals value |
| `.to_have_count(3)` | Exactly N elements match |
| `.to_be_checked()` | Checkbox/radio is checked |
| `.to_be_unchecked()` | Checkbox/radio is unchecked |
| `.to_be_attached()` | Element exists in DOM |
| `.to_be_detached()` | Element removed from DOM |

### Zero-Boilerplate Tests

The `e2e_test!` macro handles skip-when-no-server and auto-connect:

```rust
use victauri_test::{e2e_test, VictauriClient};

e2e_test!(greet_flow, |client| async move {
    client.fill_by_id("name-input", "World").await.unwrap();
    client.click_by_id("greet-btn").await.unwrap();
    client.expect_text("Hello, World!").await.unwrap();
});
```

### Managed App Lifecycle

`TestApp` starts your app, waits for the server, and cleans up on drop:

```rust
use victauri_test::TestApp;

#[tokio::test]
async fn full_lifecycle() {
    let app = TestApp::spawn("cargo run -p my-app").await.unwrap();
    let mut client = app.client().await.unwrap();

    client.click_by_text("Start").await.unwrap();
    client.expect_text("Running").await.unwrap();
    // app process is killed when `app` is dropped
}
```

---

## IPC Verification

This is what sets Victauri apart: verifying that frontend actions actually trigger the right backend commands.

### Assert IPC calls happened

```rust
use victauri_test::{assert_ipc_called, assert_ipc_called_with, assert_ipc_not_called};

// Interact with the UI
client.click_by_id("save-btn").await?;

// Verify the backend command ran
let log = client.get_ipc_log(None).await?;
assert_ipc_called(&log, "save_settings");
assert_ipc_called_with(&log, "save_settings", &json!({"theme": "dark"}));
assert_ipc_not_called(&log, "delete_account");
```

### IPC checkpoints

Isolate assertions to a specific user action:

```rust
let checkpoint = client.create_ipc_checkpoint().await?;

client.click_by_id("save-btn").await?;

let calls = client.get_ipc_calls_since(checkpoint).await?;
assert_eq!(calls.len(), 1);
assert_eq!(calls[0]["command"], "save_settings");
```

### Cross-boundary verification

Detect when the frontend and backend disagree:

```rust
let result = client.verify_state(
    "document.querySelector('.theme-label').textContent",
    json!({"theme": "dark"})
).await?;
assert!(result["divergences"].as_array().unwrap().is_empty());
```

### Ghost command detection

Find orphaned commands — called in the frontend but missing from the backend:

```rust
let ghosts = client.detect_ghost_commands().await?;
assert!(ghosts["ghost_commands"].as_array().unwrap().is_empty(),
    "Found ghost commands: {ghosts}");
```

### IPC health check

Detect stuck, stale, or errored IPC calls:

```rust
let health = client.check_ipc_integrity().await?;
assert!(health["healthy"].as_bool().unwrap());
```

---

## Fluent Verification

Check multiple conditions at once — DOM, IPC, accessibility, errors — with a single report:

```rust
let report = client.verify()
    .has_text("Settings saved")
    .has_no_text("Error")
    .ipc_was_called("save_settings")
    .ipc_was_called_with("save_settings", json!({"theme": "dark"}))
    .ipc_was_not_called("delete_account")
    .no_console_errors()
    .no_ghost_commands()
    .ipc_healthy()
    .coverage_above(80.0)
    .run()
    .await?;

report.assert_all_passed();

// Or inspect individual failures:
for failure in report.failures() {
    eprintln!("FAILED: {} — {}", failure.description, failure.detail);
}
```

---

## Visual Regression Testing

Compare screenshots against stored baselines with pixel-level diffing:

```rust
use victauri_test::visual::{VisualOptions, ThresholdPreset, MaskRegion};

// Standard tolerance (good for most apps)
let opts = VisualOptions {
    snapshot_dir: "tests/snapshots".into(),
    ..VisualOptions::from_preset(ThresholdPreset::Standard)
};

let diff = client.screenshot_visual("dashboard", &opts).await?;
assert!(diff.is_match, "visual regression: {:.2}% pixels differ", diff.diff_percentage);
```

On first run, the screenshot is saved as the baseline. Subsequent runs compare and generate a red-overlay diff image when mismatched.

**Threshold presets:**

| Preset | Tolerance | Threshold | Use case |
|---|---|---|---|
| `Strict` | 0 | 0.0% | Pixel-perfect, no variation |
| `Standard` | 2 | 0.1% | Most apps, minor anti-aliasing OK |
| `AntiAlias` | 5 | 0.5% | Cross-browser font rendering |
| `Relaxed` | 10 | 2.0% | Cross-platform CI |

**Mask regions** — exclude dynamic content from comparison:

```rust
let opts = VisualOptions {
    snapshot_dir: "tests/snapshots".into(),
    masks: vec![
        MaskRegion::new(0, 0, 200, 50),  // timestamp header
    ],
    ..VisualOptions::from_preset(ThresholdPreset::Standard)
};
```

**Save screenshots to files:**

```rust
// Save default window
client.screenshot_to_file("debug.png").await?;

// Save specific window
client.screenshot_to_file_for("main", "main-window.png").await?;
```

---

## IPC Coverage

Track which registered Tauri commands your tests actually exercise:

```rust
use victauri_test::coverage::coverage_report;

let report = coverage_report(&mut client).await?;
println!("{}", report.to_summary());
assert!(report.meets_threshold(80.0),
    "Coverage {:.1}% below 80% threshold", report.coverage_percentage);
```

Or inline with the fluent builder:

```rust
client.verify()
    .has_text("Welcome")
    .coverage_above(80.0)
    .run().await?.assert_all_passed();
```

From the CLI:

```bash
victauri coverage --threshold 80
```

**Prerequisite:** Commands must use `#[inspectable]` to be tracked. See [Command Instrumentation](#command-instrumentation).

---

## Accessibility Auditing

Run WCAG-based accessibility checks against your running app:

```rust
let audit = client.audit_accessibility().await?;
let violations = audit["summary"]["violations"].as_u64().unwrap_or(0);
assert_eq!(violations, 0, "Accessibility violations found: {audit}");
```

Checks include: images without alt text, unlabeled form inputs, empty buttons/links, heading hierarchy, color contrast (WCAG AA), ARIA role validity, positive tabindex, missing document language and title.

Use the assertion helper for a one-liner:

```rust
use victauri_test::assert_no_a11y_violations;

let audit = client.audit_accessibility().await?;
assert_no_a11y_violations(&audit);
```

---

## Performance Monitoring

Track navigation timing, memory usage, and resource loading:

```rust
let metrics = client.get_performance_metrics().await?;

// Check JS heap usage
let heap_mb = metrics["heap"]["usedJSHeapSize"]
    .as_f64().unwrap_or(0.0) / 1_048_576.0;
assert!(heap_mb < 256.0, "Heap usage too high: {heap_mb:.1} MB");

// Check page load time
let load_ms = metrics["navigation"]["loadEventEnd"]
    .as_f64().unwrap_or(0.0);
assert!(load_ms < 3000.0, "Page load too slow: {load_ms:.0}ms");
```

Or use the assertion helper with a budget:

```rust
use victauri_test::assert_performance_budget;

let metrics = client.get_performance_metrics().await?;
assert_performance_budget(&metrics, 5000.0, 512.0);  // max load ms, max heap MB
```

Metrics include: DNS lookup time, TTFB, DOM interactive/complete, load event, resource summary (count, transfer size, by type, 5 slowest), paint timing (FP, FCP), JS heap usage, long task count, DOM stats.

---

## Time-Travel Recording

Record interactions, create checkpoints, and generate test files:

### Record from the CLI

```bash
victauri record --output tests/login_flow.rs --test-name login_flow
# Interact with your app...
# Press Ctrl+C to stop and generate the test file
```

### Record programmatically

```rust
// Start recording
client.start_recording(Some("my-session")).await?;

// Create named checkpoints
client.checkpoint("before-login").await?;
client.fill_by_id("email", "user@example.com").await?;
client.click_by_id("login-btn").await?;
client.checkpoint("after-login").await?;

// Get events between checkpoints
let events = client.events_between("before-login", "after-login").await?;

// Stop and get full session
let session = client.stop_recording().await?;
```

---

## Command Instrumentation

Mark your Tauri commands with `#[inspectable]` for coverage tracking, ghost detection, and natural language resolution:

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

This generates a command schema at compile time — zero runtime cost. Commands become discoverable through `get_registry` and natural language via `resolve_command`.

To auto-discover all instrumented commands:

```rust
tauri::Builder::default()
    .plugin(
        victauri_plugin::VictauriBuilder::new()
            .auto_discover()
            .build()
            .unwrap(),
    )
    // ...
```

---

## Security & Privacy

Victauri is designed for development, not production:

- **Debug-only**: entire plugin compiles away in release builds (`#[cfg(debug_assertions)]`)
- **Localhost-only**: binds to 127.0.0.1, no remote access, DNS rebinding protection
- **Auth by default**: auto-generates a Bearer token on startup (logged for easy access)
- **Rate limited**: 1000 requests/sec default, token-bucket algorithm
- **Privacy profiles**: control exactly what agents can do

### Privacy profiles

```rust
use victauri_plugin::{VictauriBuilder, PrivacyProfile};

// Read-only: snapshots, logs — no clicks, no eval, no writes
VictauriBuilder::new()
    .privacy_profile(PrivacyProfile::Observe)
    .build()?;

// Testing: interactions + storage writes, but no raw eval or dangerous ops
VictauriBuilder::new()
    .privacy_profile(PrivacyProfile::Test)
    .build()?;

// Full control (default): everything enabled
VictauriBuilder::new()
    .privacy_profile(PrivacyProfile::FullControl)
    .build()?;
```

| Profile | DOM read | Click/fill | Storage write | Raw eval | IPC invoke |
|---|---|---|---|---|---|
| `Observe` | Yes | No | No | No | No |
| `Test` | Yes | Yes | Yes | No | Yes |
| `FullControl` | Yes | Yes | Yes | Yes | Yes |

### Command filtering and output redaction

```rust
VictauriBuilder::new()
    .command_blocklist(vec!["delete_user".into(), "drop_database".into()])
    .redact_pattern(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Z|a-z]{2,}\b")  // emails
    .redact_pattern(r"eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+")     // JWTs
    .build()?;
```

### Custom auth token

```rust
VictauriBuilder::new()
    .auth_token("my-secure-token")
    .build()?;
```

Or via environment variable: `VICTAURI_AUTH_TOKEN=my-secure-token`

To explicitly disable auth (not recommended):

```rust
VictauriBuilder::new()
    .auth_disabled()
    .build()?;
```

---

## CLI Reference

Install with `cargo install victauri-cli`, then:

```bash
victauri init                                     # Scaffold test directory with starter tests
victauri check                                    # Connect to running app, report health
victauri check --junit report.xml                 # Same, with JUnit XML output
victauri test                                     # Run 11 built-in smoke checks
victauri test --max-load-ms 5000 --max-heap-mb 256 # With custom thresholds
victauri record --output tests/flow.rs            # Record interactions → generate Rust test
victauri coverage --threshold 80                  # Report IPC coverage, fail if below 80%
victauri watch                                    # Re-run tests on file changes
victauri watch --filter smoke                     # Only re-run specific test file
```

### `victauri test` — Smoke Suite

Runs 11 built-in checks against your running app:

1. Server connectivity
2. JavaScript evaluation
3. DOM snapshot validity
4. Screenshot capture
5. Window enumeration
6. IPC integrity
7. Accessibility audit (violations)
8. Accessibility audit (warnings)
9. DOM load performance
10. Heap memory usage
11. Health endpoint hardening

Exit code 0 if all pass, 1 if any fail. Ideal for CI gates.

---

## MCP Tools

Victauri exposes 24 MCP tools — 9 compound tools (grouped actions) and 15 standalone:

### Compound tools

| Tool | Actions |
|---|---|
| **`interact`** | `click`, `double_click`, `hover`, `focus`, `scroll`, `select` — with auto-wait |
| **`input`** | `fill`, `type_text`, `press_key` — keyboard combos supported |
| **`window`** | `get_state`, `list`, `manage`, `resize`, `move`, `set_title` |
| **`storage`** | `get`, `set`, `delete`, `cookies` — localStorage + sessionStorage |
| **`navigate`** | `go_to`, `back`, `history`, `dialogs` — with auto-response config |
| **`recording`** | `start`, `stop`, `checkpoint`, `events`, `export`, `import` |
| **`inspect`** | `styles`, `bounds`, `highlight`, `audit_accessibility`, `get_performance` |
| **`logs`** | `console`, `network`, `ipc`, `navigation`, `dialogs`, `events`, `slow_ipc` |
| **`css`** | `inject`, `remove` — debug CSS injection |

### Standalone tools

| Tool | What it does |
|---|---|
| `eval_js` | Execute JavaScript in the webview |
| `dom_snapshot` | Full accessibility tree with ref handles |
| `find_elements` | Search by text, role, test ID, CSS, label, placeholder, alt, title |
| `invoke_command` | Call any Tauri command through real IPC |
| `screenshot` | Platform-native window capture (no Chromium) |
| `verify_state` | Compare frontend DOM against backend state |
| `detect_ghost_commands` | Find orphaned IPC calls |
| `check_ipc_integrity` | Detect stuck/stale/errored IPC calls |
| `wait_for` | Poll for conditions: text, selector, IPC settle |
| `assert_semantic` | Evaluate JS + assert against expected value |
| `resolve_command` | Natural language to matching Tauri command |
| `get_registry` | List all `#[inspectable]` command schemas |
| `get_memory_stats` | Real-time OS process memory statistics |
| `get_plugin_info` | Plugin config: port, tools, privacy, version |
| `get_diagnostics` | Detect edge cases: shadow DOM, service workers, iframes, large DOM |

---

## REST API

Every MCP tool is also available via plain HTTP — no MCP client, no session handshake, no protocol overhead. Use `curl`, CI scripts, or any HTTP client:

### List available tools

```bash
curl http://127.0.0.1:7373/api/tools
```

### Call a tool

```bash
# Evaluate JavaScript
curl -X POST http://127.0.0.1:7373/api/tools/eval_js \
  -H "Content-Type: application/json" \
  -d '{"code": "document.title"}'
# → {"result": "My App"}

# Get DOM snapshot
curl -X POST http://127.0.0.1:7373/api/tools/dom_snapshot \
  -d '{}'
# → {"result": {"tree": "...", "stale_refs": []}}

# Take screenshot
curl -X POST http://127.0.0.1:7373/api/tools/screenshot \
  -d '{}'
# → {"result": {"type": "image", "data": "iVBORw0KGgo...", "mimeType": "image/png"}}

# Get memory stats (no body needed)
curl -X POST http://127.0.0.1:7373/api/tools/get_memory_stats
# → {"result": {"working_set_bytes": 77000000, ...}}
```

The REST API goes through the same auth, rate-limit, and privacy middleware as MCP. If auth is enabled, add `Authorization: Bearer <token>`.

---

## VS Code Extension

Live inspection of your running Tauri app directly in VS Code. No browser DevTools, no CLI — just open your editor.

### Install

```bash
cd editors/vscode && npm install && npm run build
# Then: Ctrl+Shift+P → "Developer: Install Extension from Location..." → select editors/vscode
```

### Features

- **Activity bar** with three tree views:
  - **App State** — windows, memory, performance (JS heap, DOM stats, long tasks), plugin info, diagnostics
  - **DOM Explorer** — accessible tree with ref IDs, roles, bounds. Right-click to click, highlight, inspect styles, copy refs, generate tests
  - **IPC Log** — live command log with status, duration, timestamps
- **Screenshot panel** — inline webview with refresh and save-to-disk
- **CodeLens** — "Generate Victauri test" on every `#[tauri::command]` in Rust files
- **Commands** — Connect, Disconnect, Screenshot, Evaluate JS, Diagnostics, A11y Audit, Performance Metrics
- **Auto-connect** — detects `tauri.conf.json` in workspace, connects on activation
- **Port discovery** — reads `victauri.port` temp file for auto-fallback detection
- **Auth support** — configurable Bearer token via `victauri.authToken` setting

### Configuration

| Setting | Default | Description |
|---|---|---|
| `victauri.port` | `7373` | Port of the Victauri server |
| `victauri.autoConnect` | `true` | Auto-connect when Tauri project detected |
| `victauri.pollInterval` | `2000` | Polling interval (ms) for live updates |
| `victauri.authToken` | `""` | Bearer token for authenticated connections |

---

## CI Integration

Victauri tests run in CI without special infrastructure. Pick the approach that fits:

### Option A: GitHub Action (recommended)

```yaml
# .github/workflows/test.yml
- name: Start app
  run: xvfb-run --auto-servernum cargo run -p my-app &

- uses: runyourempire/victauri@main
  with:
    max-load-ms: 5000
    max-heap-mb: 256
    coverage: true
    coverage-threshold: 80
    junit-path: results.xml
```

One step. Installs the CLI, waits for the server, runs smoke tests, and optionally gates on IPC coverage. See [`.github/actions/victauri-test/action.yml`](.github/actions/victauri-test/action.yml) for all inputs.

### Option B: Managed lifecycle with `TestApp`

```yaml
# .github/workflows/test.yml
- name: Run Victauri tests
  run: cargo test -p my-app --test integration
```

`TestApp::spawn` handles starting the app, waiting for the server, and cleanup. Nothing else needed.

### Option C: Manual server lifecycle

```yaml
- name: Build app
  run: cargo build -p my-app

- name: Start app
  run: xvfb-run --auto-servernum cargo run -p my-app &
  # Linux needs xvfb for headless; macOS/Windows don't

- name: Wait for server
  run: |
    for i in $(seq 1 30); do
      curl -sf http://127.0.0.1:7373/health && break
      sleep 1
    done

- name: Run tests
  run: cargo test -p my-app --test integration -- --test-threads=1
  env:
    VICTAURI_E2E: "1"
    VICTAURI_PORT: "7373"
```

### Option D: Use the CLI directly

```yaml
- name: Start app
  run: xvfb-run --auto-servernum cargo run -p my-app &

- name: Smoke test
  run: victauri test --junit results.xml --max-load-ms 5000

- name: Coverage gate
  run: victauri coverage --threshold 80 --junit coverage.xml
```

### Platform notes

| Platform | Notes |
|---|---|
| Linux | Requires `xvfb-run --auto-servernum` for headless display |
| macOS | Works out of the box — no WebDriver/CDP needed |
| Windows | Works out of the box — uses native `PrintWindow` for screenshots |

---

## How It Works

```
AI Agent / cargo test / curl
        |
        v
  HTTP on :7373
  ├── /mcp          (MCP protocol — for AI agents)
  ├── /api/tools    (REST API — for scripts, CI, any HTTP client)
  └── /health       (health check — for monitoring)
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

### Why embedded matters

Victauri's MCP server runs **inside** the Tauri process — same thread pool, same memory space, direct `AppHandle` access. This isn't just an implementation detail; it changes what's possible:

| | Embedded (Victauri) | External process |
|---|---|---|
| **Tool response** | <1ms (function call) | 5–50ms (IPC + serialization) |
| **State accuracy** | Zero drift (reads live state) | Stale (snapshot + transfer) |
| **Backend access** | Full (`AppHandle`, DB, state) | Limited (webview only) |
| **Runtime deps** | None (pure Rust) | Node.js / Python / etc. |
| **Setup** | One line in `Cargo.toml` | Separate process + config |
| **Release build** | Compiles away entirely | Must be disabled manually |

External testing tools read state from outside the process through the webview. They can see the DOM but not why the DOM looks that way. Victauri sees both sides: what the frontend shows and what the backend knows. That's how it catches state drift, ghost commands, and IPC integrity issues that surface-level testing misses.

**Port selection:** Victauri tries port 7373 first, then falls back through 7374–7383 if taken. The actual port is written to a temp directory for automatic client discovery.

**JS bridge:** A persistent init script is injected into every webview. It provides DOM walking, IPC interception (via fetch monitoring), console capture, network logging, and all interaction primitives. The bridge survives page navigations in Vite dev mode.

---

## Architecture

```
victauri/
├── crates/
│   ├── victauri-core/       # Shared types (events, registry, snapshots, verification)
│   ├── victauri-macros/     # #[inspectable] proc macro for command schemas
│   ├── victauri-plugin/     # Tauri plugin + MCP server + JS bridge (the main crate)
│   ├── victauri-test/       # Test client + Locator API + assertion helpers
│   ├── victauri-cli/        # CLI: init, check, test, record, watch, coverage
│   └── victauri-watchdog/   # Health-check sidecar for crash recovery
└── examples/
    └── demo-app/            # Multi-window Tauri app with 19 instrumented commands
```

| Crate | Purpose | Tauri dependency? |
|---|---|---|
| `victauri-plugin` | Embed in your app — MCP server + bridge | Yes |
| `victauri-test` | Use in your tests — client + assertions | No |
| `victauri-cli` | Install globally — scaffold + diagnose | No |
| `victauri-macros` | Use on commands — `#[inspectable]` | No |
| `victauri-core` | Shared types (usually not used directly) | No |
| `victauri-watchdog` | Run as sidecar for crash recovery | No |

---

## Common Testing Patterns

### Pattern: Test a form submission end-to-end

```rust
#[tokio::test]
async fn submit_contact_form() {
    if !is_e2e() { return; }
    let mut client = VictauriClient::discover().await.unwrap();

    // Fill the form
    let email = Locator::label("Email");
    let message = Locator::label("Message");
    let submit = Locator::role("button").and_text("Send");

    email.fill(&mut client, "user@example.com").await.unwrap();
    message.fill(&mut client, "Hello!").await.unwrap();
    submit.click(&mut client).await.unwrap();

    // Verify UI feedback
    Locator::text("Message sent")
        .expect(&mut client)
        .to_be_visible()
        .await
        .unwrap();

    // Verify the backend actually received it
    let log = client.get_ipc_log(Some(1)).await.unwrap();
    assert_ipc_called_with(&log, "send_message", &json!({
        "email": "user@example.com",
        "body": "Hello!"
    }));
}
```

### Pattern: Test navigation between pages

```rust
#[tokio::test]
async fn navigation_flow() {
    if !is_e2e() { return; }
    let mut client = VictauriClient::discover().await.unwrap();

    Locator::text("Settings").click(&mut client).await.unwrap();

    // Wait for page transition
    Locator::role("heading").and_text("Settings")
        .expect(&mut client)
        .to_be_visible()
        .await
        .unwrap();

    // Verify URL changed (via eval)
    let url = client.eval_js("window.location.hash").await.unwrap();
    assert_eq!(url.as_str().unwrap(), "#/settings");
}
```

### Pattern: Test multi-window behavior

```rust
#[tokio::test]
async fn notification_window() {
    if !is_e2e() { return; }
    let mut client = VictauriClient::discover().await.unwrap();

    // Check which windows exist
    let windows = client.list_windows().await.unwrap();
    let labels: Vec<&str> = windows.as_array().unwrap()
        .iter().filter_map(|w| w.as_str()).collect();
    assert!(labels.contains(&"main"));

    // Get state of a specific window
    let state = client.get_window_state(Some("main")).await.unwrap();
    assert!(state["visible"].as_bool().unwrap());
}
```

### Pattern: Verify state consistency after interaction

```rust
#[tokio::test]
async fn counter_state_sync() {
    if !is_e2e() { return; }
    let mut client = VictauriClient::discover().await.unwrap();

    // Click increment 3 times
    for _ in 0..3 {
        client.click_by_id("increment-btn").await.unwrap();
    }

    // Verify frontend shows "3"
    client.expect_text("3").await.unwrap();

    // Verify backend agrees
    let result = client.invoke_command("get_counter", None).await.unwrap();
    assert_eq!(result.as_i64().unwrap(), 3);
}
```

### Pattern: Full verification report in CI

```rust
#[tokio::test]
async fn ci_health_check() {
    if !is_e2e() { return; }
    let mut client = VictauriClient::discover().await.unwrap();

    client.verify()
        .has_text("Welcome")             // UI loaded
        .no_console_errors()             // No JS errors
        .ipc_healthy()                   // No stuck IPC calls
        .no_ghost_commands()             // No orphaned commands
        .coverage_above(75.0)            // Command coverage
        .run().await.unwrap()
        .assert_all_passed();
}
```

---

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| Server won't start | Port 7373 in use | The plugin auto-falls-back to 7374–7383. Check `victauri check` for the actual port. |
| "Connection refused" | App not running | Start your app first: `pnpm tauri dev` |
| Auth token mismatch | Token not propagated | Use `VictauriClient::discover()` (reads token from temp files) or set `VICTAURI_AUTH_TOKEN` |
| Tests interfere | Shared server state | Run with `--test-threads=1` — Victauri uses one server per app process |
| Linux CI: "no display" | Missing display server | Wrap with `xvfb-run --auto-servernum` |
| 0% coverage | No `#[inspectable]` commands | Add `#[inspectable]` to your Tauri commands and call `.auto_discover()` |
| Bridge not loading | CSP blocks scripts | Check that your CSP allows inline scripts, or use Vite's dev mode (no CSP) |
| Eval timeout (30s) | Heavy JS / blocked thread | Check for infinite loops or long-running sync operations in your webview |
| Plugin not found | Wrong Cargo.toml section | `victauri-plugin` should be under `[dependencies]`, not `[dev-dependencies]` (it needs to run inside your app process) |
| Release build: no server | Working as designed | Victauri is gated behind `#[cfg(debug_assertions)]` — release builds have zero overhead |

---

## Documentation

- [**Testing Tauri Apps**](docs/testing-tauri-apps.md) — comprehensive guide covering every testing approach (unit tests, frontend mocks, WebDriver, Playwright, Victauri)
- [**Compatibility**](docs/compatibility.md) — CSP requirements, IPC pattern support, multi-window handling, tested apps
- [**VS Code Extension**](editors/vscode/) — live app inspection, DOM explorer, IPC log, CodeLens test generation
- [**Agent Session Example**](examples/agent-session.md) — real AI agent session transcript
- [**Demo App Tests**](examples/demo-app/tests/integration.rs) — 20 integration tests demonstrating every pattern
- [**Migration Guide**](MIGRATION.md) — upgrading between versions
- [**Contributing**](CONTRIBUTING.md) — how to contribute

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

**Lint policy:** 20 clippy lints (pedantic + nursery) are enforced at `deny` level workspace-wide — see `[workspace.lints.clippy]` in `Cargo.toml`. PRs that introduce warnings won't compile.

## License

Apache-2.0 — [LICENSE](LICENSE)

Built and maintained by [4DA Systems](https://4da.ai).
