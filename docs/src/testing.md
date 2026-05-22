# Testing

Victauri provides a complete testing toolkit: a typed HTTP client, assertion helpers, a built-in smoke suite, and a CLI for running tests from the terminal.

## victauri-test Crate

Add the test crate to your dev dependencies:

```toml
[dev-dependencies]
victauri-test = "0.2"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

### VictauriClient

The `VictauriClient` is a typed HTTP client that handles MCP session lifecycle automatically:

```rust
use victauri_test::VictauriClient;

#[tokio::test]
async fn test_my_app() {
    // Connect (handles MCP initialize + notifications/initialized handshake)
    let client = VictauriClient::connect(7373).await.unwrap();
    
    // Evaluate JavaScript
    let title = client.eval_js("document.title").await.unwrap();
    assert_eq!(title, "\"My App\"");
    
    // Take a DOM snapshot
    let snapshot = client.dom_snapshot().await.unwrap();
    assert!(snapshot["children"].as_array().unwrap().len() > 0);
    
    // Click an element
    client.click("e3").await.unwrap();
    
    // Fill an input
    client.fill("e5", "hello@example.com").await.unwrap();
    
    // Type text character-by-character
    client.type_text("e5", "search query").await.unwrap();
    
    // Take a screenshot
    let png_base64 = client.screenshot(None).await.unwrap();
    assert!(png_base64.starts_with("iVBOR")); // PNG magic bytes in base64
}
```

### With Authentication

```rust
let client = VictauriClient::connect_with_token(7373, "my-secret-token")
    .await
    .unwrap();
```

### Available Client Methods

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

## Assertion Helpers

Standalone functions for common verification patterns:

```rust
use victauri_test::{
    assert_json_eq,
    assert_json_truthy,
    assert_no_a11y_violations,
    assert_performance_budget,
    assert_ipc_healthy,
    assert_state_matches,
};

#[tokio::test]
async fn test_assertions() {
    let client = VictauriClient::connect(7373).await.unwrap();
    
    // Verify eval result matches expected value
    assert_json_eq(&client, "document.title", "My App").await;
    
    // Verify expression is truthy
    assert_json_truthy(&client, "document.querySelector('nav')").await;
    
    // Run accessibility audit and fail on violations
    assert_no_a11y_violations(&client).await;
    
    // Check performance budgets
    assert_performance_budget(&client, 100.0, 50.0).await; // heap MB, DOM load ms
    
    // Verify IPC health
    assert_ipc_healthy(&client).await;
    
    // Cross-boundary state comparison
    assert_state_matches(&client, "document.title", json!({"title": "My App"})).await;
}
```

### Client Assertion Methods

The client also has built-in assertion methods:

```rust
// Verify basic functionality
client.assert_eval_works().await;
client.assert_dom_snapshot_valid().await;
client.assert_screenshot_ok().await;

// Verify infrastructure
client.assert_windows_exist(&["main"]).await;
client.assert_ipc_integrity_ok().await;
client.assert_accessible().await; // WCAG audit

// Performance assertions
client.assert_dom_complete_under(5000).await; // ms
client.assert_heap_under_mb(200.0).await;
client.assert_no_uncaught_errors().await;

// Full lifecycle verification
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
    
    // Run with default thresholds
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

The smoke suite checks: health endpoint, eval, DOM snapshot, screenshot, window state, IPC integrity, memory, accessibility, performance, recording lifecycle, and error handling.

Reports include timing data and can export to JUnit XML for CI integration.

## victauri-cli

The CLI provides test commands without writing Rust code:

```bash
cargo install victauri-cli
```

### Commands

#### `victauri init`

Scaffold a test directory in your project:

```bash
victauri init
```

Creates a `tests/victauri/` directory with example test files.

#### `victauri check`

Run diagnostics against a running app:

```bash
victauri check
# Checks: health, info, auth, tools available, bridge connected
```

#### `victauri test`

Run the built-in smoke suite:

```bash
victauri test
# Runs 11 checks, prints pass/fail summary, exits 0 or 1

# Custom thresholds
victauri test --max-load-ms 5000 --max-heap-mb 200
```

Output includes a pass/fail summary suitable for CI. Also generates JUnit XML for test reporting tools.

#### `victauri record`

Capture interactions for test generation:

```bash
victauri record --output test_login.json
# Records all events until you stop (Ctrl+C)
# Exports a session file that can be replayed
```

#### `victauri watch`

File watcher that re-runs tests on change:

```bash
victauri watch
# Watches your test files and reruns on save
```

#### `victauri coverage`

IPC command coverage report:

```bash
victauri coverage
# Shows which registered commands have been exercised
# via IPC during the current session
```

## Integration Test Patterns

### Basic Tool Verification

```rust
#[tokio::test]
async fn test_eval_and_snapshot() {
    let client = VictauriClient::connect(7373).await.unwrap();
    
    let title = client.eval_js("document.title").await.unwrap();
    assert!(title.contains("My App"));
    
    let snap = client.dom_snapshot().await.unwrap();
    let elements = snap["children"].as_array().unwrap();
    assert!(!elements.is_empty());
}
```

### Interaction Flow

```rust
#[tokio::test]
async fn test_form_submission() {
    let client = VictauriClient::connect(7373).await.unwrap();
    
    // Find the input
    let elements = client.find_elements("input[name='email']").await.unwrap();
    let ref_id = elements[0]["ref"].as_str().unwrap();
    
    // Fill and submit
    client.fill(ref_id, "test@example.com").await.unwrap();
    client.press_key("Enter").await.unwrap();
    
    // Wait for result
    client.wait_for("selector", ".success-message", Some(3000)).await.unwrap();
}
```

### Cross-Boundary Verification

```rust
#[tokio::test]
async fn test_state_consistency() {
    let client = VictauriClient::connect(7373).await.unwrap();
    
    // Invoke a backend command
    let settings = client.invoke_command("get_settings", json!({})).await.unwrap();
    
    // Verify frontend matches backend
    let result = client.verify_state(
        Some("document.title"),
        Some(json!({"title": settings["app_name"]})),
    ).await.unwrap();
    
    assert_eq!(result["passed"], true);
    assert_eq!(result["divergences"].as_array().unwrap().len(), 0);
}
```

### Time-Travel Recording

```rust
#[tokio::test]
async fn test_with_recording() {
    let client = VictauriClient::connect(7373).await.unwrap();
    
    // Start recording
    client.start_recording().await.unwrap();
    client.checkpoint("initial").await.unwrap();
    
    // Perform actions
    client.click("e3").await.unwrap();
    client.checkpoint("after-click").await.unwrap();
    
    // Stop and examine
    let session = client.stop_recording().await.unwrap();
    let events = session["events"].as_array().unwrap();
    assert!(!events.is_empty());
}
```
