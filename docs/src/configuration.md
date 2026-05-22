# Configuration

Victauri is configured via the `VictauriBuilder` API in Rust code and/or environment variables.

## Quick Reference

| Setting | Builder Method | Environment Variable | Default |
|---------|---------------|---------------------|---------|
| Port | `.port(7373)` | `VICTAURI_PORT` | 7373 |
| Auth token | `.auth_token("...")` | `VICTAURI_AUTH_TOKEN` | Auto-generated UUID |
| Disable auth | `.auth_disabled()` | — | Auth enabled |
| Eval timeout | `.eval_timeout(Duration)` | `VICTAURI_EVAL_TIMEOUT` | 30s |
| Event capacity | `.event_capacity(10000)` | — | 10,000 |
| Recorder capacity | `.recorder_capacity(50000)` | — | 50,000 |
| Console log cap | `.console_log_capacity(1000)` | — | 1,000 |
| Network log cap | `.network_log_capacity(1000)` | — | 1,000 |
| Navigation log cap | `.navigation_log_capacity(200)` | — | 200 |

## VictauriBuilder API

### Basic Setup

```rust
use victauri_plugin::VictauriBuilder;

tauri::Builder::default()
    .plugin(
        VictauriBuilder::new()
            .port(8080)
            .auth_token("my-fixed-token")
            .build(),
    )
    .run(tauri::generate_context!())
    .unwrap();
```

### Port Configuration

```rust
VictauriBuilder::new()
    .port(9000)  // Preferred port
    .build()
```

If the preferred port is busy, Victauri tries the next 10 ports (9001-9010). The actual port is:
- Printed to the log on startup
- Written to `<temp_dir>/victauri.port`
- Available via the `/info` endpoint
- Stored in `VictauriState.port` (AtomicU16)

### Authentication

Authentication is **enabled by default**. Three modes:

```rust
// 1. Auto-generated token (default — token printed to console)
VictauriBuilder::new().build()

// 2. Fixed token
VictauriBuilder::new()
    .auth_token("my-secret-token")
    .build()

// 3. Random UUID token (explicit)
VictauriBuilder::new()
    .generate_auth_token()
    .build()

// 4. No authentication (use only in trusted environments)
VictauriBuilder::new()
    .auth_disabled()
    .build()
```

The `VICTAURI_AUTH_TOKEN` environment variable overrides any programmatic token.

### Privacy Controls

#### Privacy Profiles

Three tiers of access control:

```rust
use victauri_plugin::PrivacyProfile;

// Read-only: snapshots, logs, registry only. No mutations.
VictauriBuilder::new()
    .privacy_profile(PrivacyProfile::Observe)
    .build()

// Testing: observe + interactions + input + storage + recording
VictauriBuilder::new()
    .privacy_profile(PrivacyProfile::Test)
    .build()

// Full control: everything enabled (default)
VictauriBuilder::new()
    .privacy_profile(PrivacyProfile::FullControl)
    .build()
```

`Observe` and `Test` profiles automatically enable output redaction.

#### Strict Privacy Mode

Shorthand for `Observe` profile:

```rust
VictauriBuilder::new()
    .strict_privacy_mode()
    .build()
```

#### Tool Disabling

Disable specific tools by name:

```rust
VictauriBuilder::new()
    .disable_tools(&["eval_js", "screenshot", "invoke_command"])
    .build()
```

Disabled tools return an error when called and are not listed in tool discovery.

#### Command Allowlists and Blocklists

Control which Tauri commands can be invoked via MCP:

```rust
// Only allow these commands (positive allowlist)
VictauriBuilder::new()
    .command_allowlist(&["get_settings", "get_status", "search"])
    .build()

// Block specific commands (negative blocklist)
VictauriBuilder::new()
    .command_blocklist(&["delete_user", "reset_database", "admin_override"])
    .build()
```

The allowlist takes priority: if set, only listed commands are permitted regardless of the blocklist.

### Output Redaction

Automatically redact sensitive data from tool responses:

```rust
VictauriBuilder::new()
    .enable_redaction()  // Built-in patterns: API keys, emails, tokens
    .add_redaction_pattern(r"SECRET_\w+")  // Custom regex
    .add_redaction_pattern(r"sk-[a-zA-Z0-9]+")  // OpenAI keys
    .build()
```

Built-in patterns match:
- API keys (`api_key`, `apikey`, `api-key` in JSON)
- Bearer tokens
- Email addresses
- Common secret patterns

### Capacity Tuning

```rust
VictauriBuilder::new()
    .event_capacity(50_000)       // Ring buffer for event log (max: 1,000,000)
    .recorder_capacity(100_000)   // Time-travel recording buffer (max: 1,000,000)
    .eval_timeout(std::time::Duration::from_secs(60))  // JS eval timeout (max: 300s)
    .console_log_capacity(2000)   // JS bridge console buffer
    .network_log_capacity(2000)   // JS bridge network buffer
    .navigation_log_capacity(500) // JS bridge navigation buffer
    .build()
```

### File Navigation

By default, the `navigate` tool only allows `http://` and `https://` URLs. To allow `file://` URLs:

```rust
VictauriBuilder::new()
    .allow_file_navigation()
    .build()
```

### Ready Callback

Get notified when the server is bound and ready:

```rust
VictauriBuilder::new()
    .on_ready(|port| {
        println!("Victauri ready on port {}", port);
    })
    .build()
```

### Pre-registering Commands

Register `#[inspectable]` command schemas at build time:

```rust
VictauriBuilder::new()
    .register_command(greet__schema())
    .register_command(increment__schema())
    .build()
```

## Environment Variables

| Variable | Description |
|----------|-------------|
| `VICTAURI_PORT` | Override the MCP server port |
| `VICTAURI_AUTH_TOKEN` | Set the authentication token |
| `VICTAURI_EVAL_TIMEOUT` | Eval timeout in seconds |

Environment variables take priority over builder settings.

## Watchdog Configuration

The `victauri-watchdog` binary is configured entirely via environment variables:

| Variable | Default | Description |
|----------|---------|-------------|
| `VICTAURI_PORT` | 7373 | Port to monitor |
| `VICTAURI_INTERVAL` | 5 | Health check interval in seconds |
| `VICTAURI_MAX_FAILURES` | 3 | Consecutive failures before recovery action |
| `VICTAURI_ON_FAILURE` | (none) | Shell command to execute on failure |

```bash
VICTAURI_PORT=7373 VICTAURI_MAX_FAILURES=5 VICTAURI_ON_FAILURE="notify-send 'App crashed'" victauri-watchdog
```

## Full Example

```rust
use std::time::Duration;
use victauri_plugin::{VictauriBuilder, PrivacyProfile};

tauri::Builder::default()
    .plugin(
        VictauriBuilder::new()
            // Network
            .port(7373)
            .eval_timeout(Duration::from_secs(30))
            // Auth
            .auth_token("dev-token-123")
            // Privacy
            .privacy_profile(PrivacyProfile::Test)
            .command_blocklist(&["dangerous_command"])
            .disable_tools(&["screenshot"])
            // Redaction
            .enable_redaction()
            .add_redaction_pattern(r"password=\w+")
            // Capacity
            .event_capacity(20_000)
            .recorder_capacity(100_000)
            .console_log_capacity(2000)
            // Commands
            .register_command(greet__schema())
            .register_command(increment__schema())
            // Callback
            .on_ready(|port| println!("MCP server on :{}", port))
            .build(),
    )
    .invoke_handler(tauri::generate_handler![greet, increment])
    .run(tauri::generate_context!())
    .unwrap();
```
