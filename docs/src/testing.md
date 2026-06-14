# Testing

Victauri provides a complete testing toolkit: a typed HTTP client, a Locator API with auto-waiting, assertion helpers, a fluent verification builder, visual regression testing, IPC coverage tracking, and a CLI for running tests from the terminal.

## Quick Start

Add the test crate to your dev dependencies:

```toml
[dev-dependencies]
victauri-test = "0.5"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Write a test:

```rust
use victauri_test::{e2e_test, VictauriClient};

e2e_test!(greet_flow, |client| async move {
    client.fill_by_id("name-input", "World").await.unwrap();
    client.click_by_id("greet-btn").await.unwrap();
    client.expect_text("Hello, World!").await.unwrap();
});
```

Run it:

```bash
pnpm tauri dev                                   # start your app
VICTAURI_E2E=1 cargo test --test smoke           # run tests
```

## Test Client

### VictauriClient

The `VictauriClient` is a typed HTTP client that handles MCP session lifecycle automatically:

```rust
use victauri_test::VictauriClient;

#[tokio::test]
async fn test_my_app() {
    let client = VictauriClient::connect(7373).await.unwrap();

    let title = client.eval_js("document.title").await.unwrap();
    assert!(title.contains("My App"));

    client.click("e3").await.unwrap();
    client.fill("e5", "hello@example.com").await.unwrap();
}
```

### Auto-Discovery

`discover()` reads the port and auth token from temp files written by the plugin:

```rust
let mut client = VictauriClient::discover().await.unwrap();
```

### With Authentication

```rust
let client = VictauriClient::connect_with_token(7373, "my-secret-token")
    .await
    .unwrap();
```

### Direct Client Methods

High-level methods that find elements by text or ID — no ref handles or selectors needed:

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
| `double_click_by_id("item")` | Find element by id, double-click it |
| `hover("e3")` | Hover over element by ref |
| `scroll_to_by_id("footer")` | Scroll element into viewport |

### Low-Level Client Methods

For direct MCP tool access using ref handles:

| Method | Description |
|--------|-------------|
| `eval_js(expr)` | Evaluate JavaScript |
| `dom_snapshot()` | Get full DOM tree |
| `find_elements(selector)` | Find elements by CSS |
| `click(ref_id)` | Click element |
| `fill(ref_id, value)` | Fill input |
| `type_text(ref_id, text)` | Type characters |
| `press_key(key)` | Press keyboard key |
| `screenshot(label)` | Capture PNG |
| `get_window_state(label)` | Window position/size |
| `list_windows()` | All window labels |
| `invoke_command(name, args)` | Call Tauri command |
| `get_ipc_log(limit)` | IPC call history |
| `get_registry()` | Registered commands |
| `get_memory_stats()` | Process memory |
| `verify_state(frontend, backend)` | Cross-boundary check |
| `detect_ghost_commands()` | Unregistered commands |
| `check_ipc_integrity()` | IPC health |
| `assert_semantic(expr, cond, expected)` | Semantic assertion |
| `wait_for(condition, value, timeout)` | Wait for condition |
| `start_recording()` | Begin time-travel |
| `stop_recording()` | End recording |
| `checkpoint(label)` | Create checkpoint |
| `get_console_logs(since)` | Console entries |
| `audit_accessibility()` | WCAG audit |
| `get_performance_metrics()` | Navigation timing, heap, resources |
| `query_db(sql, db_path, params)` | SQLite query |
| `app_info()` | App config and paths |

## Locator API

For complex queries, Victauri provides composable locators with auto-waiting expectations.

### Factory Methods

Create locators by different strategies:

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

### Refinement

Narrow results with chained filters:

```rust
// Button with role "button" AND text containing "Save"
let save = Locator::role("button").and_text("Save");

// Third item in a list
let third = Locator::role("listitem").nth(2);

// Input with specific tag
let textarea = Locator::label("Description").and_tag("textarea");
```

### Actions

Interact with resolved elements:

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

### Queries

Read element state:

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

### Expectations

Auto-waiting assertions with configurable timeout:

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

### Full Locator Example

```rust
use victauri_test::prelude::*;

#[tokio::test]
async fn settings_flow() {
    if !is_e2e() { return; }
    let mut client = VictauriClient::discover().await.unwrap();

    let save_btn = Locator::role("button").and_text("Save");
    let email = Locator::label("Email address");
    let toast = Locator::test_id("toast-message");

    email.fill(&mut client, "user@example.com").await.unwrap();
    save_btn.click(&mut client).await.unwrap();

    toast.expect(&mut client)
        .to_contain_text("Settings saved")
        .await
        .unwrap();

    toast.expect(&mut client)
        .timeout_ms(10_000)
        .not()
        .to_be_visible()
        .await
        .unwrap();
}
```

## Zero-Boilerplate Tests

The `e2e_test!` macro handles skip-when-no-server and auto-connect:

```rust
use victauri_test::{e2e_test, VictauriClient};

e2e_test!(greet_flow, |client| async move {
    client.fill_by_id("name-input", "World").await.unwrap();
    client.click_by_id("greet-btn").await.unwrap();
    client.expect_text("Hello, World!").await.unwrap();
});
```

## Managed App Lifecycle

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

## IPC Verification

### Assert IPC Calls Happened

```rust
use victauri_test::{assert_ipc_called, assert_ipc_called_with, assert_ipc_not_called};

client.click_by_id("save-btn").await?;

let log = client.get_ipc_log(None).await?;
assert_ipc_called(&log, "save_settings");
assert_ipc_called_with(&log, "save_settings", &json!({"theme": "dark"}));
assert_ipc_not_called(&log, "delete_account");
```

### IPC Checkpoints

Isolate assertions to a specific user action:

```rust
let checkpoint = client.create_ipc_checkpoint().await?;

client.click_by_id("save-btn").await?;

let calls = client.get_ipc_calls_since(checkpoint).await?;
assert_eq!(calls.len(), 1);
assert_eq!(calls[0]["command"], "save_settings");
```

### Cross-Boundary Verification

Detect when the frontend and backend disagree:

```rust
let result = client.verify_state(
    "document.querySelector('.theme-label').textContent",
    json!({"theme": "dark"})
).await?;
assert!(result["divergences"].as_array().unwrap().is_empty());
```

### Ghost Command Detection

Find orphaned commands — called in the frontend but missing from the backend:

```rust
let ghosts = client.detect_ghost_commands().await?;
assert!(ghosts["ghost_commands"].as_array().unwrap().is_empty(),
    "Found ghost commands: {ghosts}");
```

### IPC Health Check

Detect stuck, stale, or errored IPC calls:

```rust
let health = client.check_ipc_integrity().await?;
assert!(health["healthy"].as_bool().unwrap());
```

## Backend Access

Victauri provides direct access to the Rust backend — no webview proxy needed.

### Query SQLite Databases

```rust
let result = client.query_db(
    "SELECT * FROM users WHERE active = ?",
    None,                           // auto-discover database
    Some(vec![json!(true)]),        // bind parameters
).await?;
println!("{} rows", result["row_count"]);
for row in result["rows"].as_array().unwrap() {
    println!("  {} ({})", row["name"], row["email"]);
}
```

### Inspect App Configuration

```rust
let info = client.app_info().await?;
println!("App: {}", info["config"]["product_name"]);
println!("Data dir: {}", info["paths"]["data"]);
println!("Databases found: {:?}", info["databases"]);
```

### Browse and Read Backend Files

```rust
let files = client.list_app_dir(Some("data"), None).await?;
for entry in files["entries"].as_array().unwrap() {
    println!("  {} ({} bytes)", entry["name"], entry["size"]);
}

let content = client.read_app_file("settings.json", Some("config")).await?;
println!("{}", content["content"]);
```

### End-to-End: UI Action to Database Verification

```rust
client.click_by_id("save-btn").await?;

let log = client.get_ipc_log(None).await?;
assert_ipc_called(&log, "save_settings");

let result = client.query_db(
    "SELECT value FROM settings WHERE key = 'theme'",
    None, None,
).await?;
assert_eq!(result["rows"][0]["value"], "dark");
```

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

for failure in report.failures() {
    eprintln!("FAILED: {} — {}", failure.description, failure.detail);
}
```

## Visual Regression Testing

Compare screenshots against stored baselines with pixel-level diffing:

```rust
use victauri_test::visual::{VisualOptions, ThresholdPreset, MaskRegion};

let opts = VisualOptions {
    snapshot_dir: "tests/snapshots".into(),
    ..VisualOptions::from_preset(ThresholdPreset::Standard)
};

let diff = client.screenshot_visual("dashboard", &opts).await?;
assert!(diff.is_match, "visual regression: {:.2}% pixels differ", diff.diff_percentage);
```

On first run, the screenshot is saved as the baseline. Subsequent runs compare and generate a red-overlay diff image when mismatched.

### Threshold Presets

| Preset | Tolerance | Threshold | Use case |
|---|---|---|---|
| `Strict` | 0 | 0.0% | Pixel-perfect, no variation |
| `Standard` | 2 | 0.1% | Most apps, minor anti-aliasing OK |
| `AntiAlias` | 5 | 0.5% | Cross-browser font rendering |
| `Relaxed` | 10 | 2.0% | Cross-platform CI |

### Mask Regions

Exclude dynamic content from comparison:

```rust
let opts = VisualOptions {
    snapshot_dir: "tests/snapshots".into(),
    masks: vec![
        MaskRegion::new(0, 0, 200, 50),  // timestamp header
    ],
    ..VisualOptions::from_preset(ThresholdPreset::Standard)
};
```

### Save Screenshots to Files

```rust
client.screenshot_to_file("debug.png").await?;
client.screenshot_to_file_for("main", "main-window.png").await?;
```

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

## Performance Monitoring

Track navigation timing, memory usage, and resource loading:

```rust
let metrics = client.get_performance_metrics().await?;

let heap_mb = metrics["heap"]["usedJSHeapSize"]
    .as_f64().unwrap_or(0.0) / 1_048_576.0;
assert!(heap_mb < 256.0, "Heap usage too high: {heap_mb:.1} MB");

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

## Time-Travel Recording

Record interactions, create checkpoints, and generate test files.

### Record from the CLI

```bash
victauri record --output tests/login_flow.rs --test-name login_flow
# Interact with your app...
# Press Ctrl+C to stop and generate the test file
```

### Record Programmatically

```rust
client.start_recording(Some("my-session")).await?;

client.checkpoint("before-login").await?;
client.fill_by_id("email", "user@example.com").await?;
client.click_by_id("login-btn").await?;
client.checkpoint("after-login").await?;

let events = client.events_between("before-login", "after-login").await?;

let session = client.stop_recording().await?;
```

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

## Assertion Helpers

### Standalone Functions

```rust
use victauri_test::{
    assert_json_eq,
    assert_json_truthy,
    assert_no_a11y_violations,
    assert_performance_budget,
    assert_ipc_healthy,
    assert_state_matches,
};

assert_json_eq(&client, "document.title", "My App").await;
assert_json_truthy(&client, "document.querySelector('nav')").await;
assert_no_a11y_violations(&client).await;
assert_performance_budget(&client, 100.0, 50.0).await;
assert_ipc_healthy(&client).await;
assert_state_matches(&client, "document.title", json!({"title": "My App"})).await;
```

### Client Assertion Methods

```rust
client.assert_eval_works().await;
client.assert_dom_snapshot_valid().await;
client.assert_screenshot_ok().await;
client.assert_windows_exist(&["main"]).await;
client.assert_ipc_integrity_ok().await;
client.assert_accessible().await;
client.assert_dom_complete_under(5000).await;
client.assert_heap_under_mb(200.0).await;
client.assert_no_uncaught_errors().await;
client.assert_recording_lifecycle().await;
client.assert_health_hardened().await;
```

## Smoke Test Suite

Run the built-in 11-check smoke test programmatically:

```rust
use victauri_test::{VictauriClient, SmokeConfig};

#[tokio::test]
async fn smoke() {
    let client = VictauriClient::connect(7373).await.unwrap();

    let report = client.smoke_test(SmokeConfig::default()).await;
    println!("Passed: {}/{}", report.passed, report.total);
    assert!(report.all_passed());

    // Custom thresholds
    let config = SmokeConfig {
        max_load_ms: 3000,
        max_heap_mb: 150.0,
        ..Default::default()
    };
    let report = client.smoke_test(config).await;
}
```

The 11 checks: health endpoint, eval, DOM snapshot, screenshot, window state, IPC integrity, memory, accessibility (violations), accessibility (warnings), performance, and health endpoint hardening.

Reports include timing data and can export to JUnit XML for CI integration.

## Common Patterns

### Test a Form Submission End-to-End

```rust
#[tokio::test]
async fn submit_contact_form() {
    if !is_e2e() { return; }
    let mut client = VictauriClient::discover().await.unwrap();

    let email = Locator::label("Email");
    let message = Locator::label("Message");
    let submit = Locator::role("button").and_text("Send");

    email.fill(&mut client, "user@example.com").await.unwrap();
    message.fill(&mut client, "Hello!").await.unwrap();
    submit.click(&mut client).await.unwrap();

    Locator::text("Message sent")
        .expect(&mut client)
        .to_be_visible()
        .await
        .unwrap();

    let log = client.get_ipc_log(Some(1)).await.unwrap();
    assert_ipc_called_with(&log, "send_message", &json!({
        "email": "user@example.com",
        "body": "Hello!"
    }));
}
```

### Test Navigation Between Pages

```rust
#[tokio::test]
async fn navigation_flow() {
    if !is_e2e() { return; }
    let mut client = VictauriClient::discover().await.unwrap();

    Locator::text("Settings").click(&mut client).await.unwrap();

    Locator::role("heading").and_text("Settings")
        .expect(&mut client)
        .to_be_visible()
        .await
        .unwrap();

    let url = client.eval_js("window.location.hash").await.unwrap();
    assert_eq!(url.as_str().unwrap(), "#/settings");
}
```

### Test Multi-Window Behavior

```rust
#[tokio::test]
async fn notification_window() {
    if !is_e2e() { return; }
    let mut client = VictauriClient::discover().await.unwrap();

    let windows = client.list_windows().await.unwrap();
    let labels: Vec<&str> = windows.as_array().unwrap()
        .iter().filter_map(|w| w.as_str()).collect();
    assert!(labels.contains(&"main"));

    let state = client.get_window_state(Some("main")).await.unwrap();
    assert!(state["visible"].as_bool().unwrap());
}
```

### Verify State Consistency After Interaction

```rust
#[tokio::test]
async fn counter_state_sync() {
    if !is_e2e() { return; }
    let mut client = VictauriClient::discover().await.unwrap();

    for _ in 0..3 {
        client.click_by_id("increment-btn").await.unwrap();
    }

    client.expect_text("3").await.unwrap();

    let result = client.invoke_command("get_counter", None).await.unwrap();
    assert_eq!(result.as_i64().unwrap(), 3);
}
```

### Full Verification Report in CI

```rust
#[tokio::test]
async fn ci_health_check() {
    if !is_e2e() { return; }
    let mut client = VictauriClient::discover().await.unwrap();

    client.verify()
        .has_text("Welcome")
        .no_console_errors()
        .ipc_healthy()
        .no_ghost_commands()
        .coverage_above(75.0)
        .run().await.unwrap()
        .assert_all_passed();
}
```

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

## CI Integration

Victauri tests run in CI without special infrastructure. Pick the approach that fits:

### Option A: GitHub Action (recommended)

```yaml
# .github/workflows/test.yml
- name: Start app
  run: xvfb-run --auto-servernum cargo run -p my-app &

- uses: runyourempire/victauri/.github/actions/victauri-test@v0.8.1
  with:
    max-load-ms: 5000
    max-heap-mb: 256
    coverage: true
    coverage-threshold: 80
    junit-path: results.xml
```

One step. Installs the CLI, waits for the server, runs smoke tests, and optionally gates on IPC coverage.

### Option B: Managed Lifecycle with TestApp

```yaml
# .github/workflows/test.yml
- name: Run Victauri tests
  run: cargo test -p my-app --test integration
```

`TestApp::spawn` handles starting the app, waiting for the server, and cleanup. Nothing else needed.

### Option C: Manual Server Lifecycle

```yaml
- name: Build app
  run: cargo build -p my-app

- name: Start app
  run: xvfb-run --auto-servernum cargo run -p my-app &

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

### Option D: Use the CLI Directly

```yaml
- name: Start app
  run: xvfb-run --auto-servernum cargo run -p my-app &

- name: Smoke test
  run: victauri test --junit results.xml --max-load-ms 5000

- name: Coverage gate
  run: victauri coverage --threshold 80 --junit coverage.xml
```

### Platform Notes

| Platform | Notes |
|---|---|
| Linux | Requires `xvfb-run --auto-servernum` for headless display |
| macOS | Works out of the box — no WebDriver/CDP needed |
| Windows | Works out of the box — uses native `PrintWindow` for screenshots |
