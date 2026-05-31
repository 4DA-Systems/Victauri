# Getting Started

Get Victauri running in your Tauri app in under 5 minutes.

## Prerequisites

- A Tauri 2.0+ application
- Rust toolchain (stable)
- An MCP client (Claude Code, VS Code, or any MCP-compatible tool)

## Step 1: Add the Dependency

Add `victauri-plugin` to your app's `src-tauri/Cargo.toml`:

```toml
[dependencies]
victauri-plugin = "0.5"
```

The plugin must be a regular dependency (not `[dev-dependencies]`) because it runs inside your app process. In release builds, `init()` returns a no-op plugin with zero overhead — no feature flags needed.

## Step 2: Initialize the Plugin

Add `victauri::init()` to your Tauri builder in `src-tauri/src/main.rs`:

```rust
fn main() {
    tauri::Builder::default()
        .plugin(victauri_plugin::init())
        // ... your other plugins and setup
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

That's it. In debug builds, this starts an MCP server on `127.0.0.1:7373`. In release builds, it's a no-op.

## Step 3: Add Capabilities

Add the `victauri:default` capability to your app's capabilities file. Create or edit `src-tauri/capabilities/default.json`:

```json
{
  "identifier": "default",
  "windows": ["*"],
  "permissions": [
    "core:default",
    "victauri:default"
  ]
}
```

Without this capability, the Tauri permission system silently blocks IPC callbacks and the plugin cannot interact with your webviews.

## Step 4: Connect via MCP

Once your app is running in debug mode, the MCP server is available at:

```
http://127.0.0.1:7373/mcp
```

### Claude Code Connection

Create a `.mcp.json` file in your project root:

```json
{
  "mcpServers": {
    "victauri": {
      "url": "http://127.0.0.1:7373/mcp"
    }
  }
}
```

Claude Code will automatically discover and connect to your running app.

### With Authentication

Auth is disabled by default for zero-friction setup. For shared machines or CI, enable it:

```rust
tauri::Builder::default()
    .plugin(
        victauri_plugin::VictauriBuilder::new()
            .auth_token("my-secret-token")
            .build(),
    )
    .run(tauri::generate_context!())
    .unwrap();
```

Then include it in your `.mcp.json`:

```json
{
  "mcpServers": {
    "victauri": {
      "url": "http://127.0.0.1:7373/mcp",
      "headers": {
        "Authorization": "Bearer my-secret-token"
      }
    }
  }
}
```

## Step 5: Verify It Works

With your app running, check the health endpoint:

```bash
curl http://127.0.0.1:7373/health
# Returns: ok

curl http://127.0.0.1:7373/info
# Returns: {"name":"victauri","port":7373,"protocol":"mcp","version":"0.8.0",...}
```

Or use the Victauri CLI:

```bash
cargo install victauri-cli
victauri check
```

## Optional: Register Commands

To enable command discovery and ghost command detection, annotate your Tauri commands with `#[inspectable]` and register them:

```rust
use victauri_plugin::inspectable;

#[inspectable(description = "Greet a user", intent = "greeting")]
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

fn main() {
    tauri::Builder::default()
        .plugin(victauri_plugin::init())
        .invoke_handler(tauri::generate_handler![greet])
        .setup(|app| {
            victauri_plugin::register_commands!(app, greet__schema());
            Ok(())
        })
        .run(tauri::generate_context!())
        .unwrap();
}
```

## Optional: REST API

All 33 tools are also available via a REST API without MCP session overhead:

```bash
# List available tools
curl http://127.0.0.1:7373/api/tools

# Execute a tool directly
curl -X POST http://127.0.0.1:7373/api/tools/eval_js \
  -H "Content-Type: application/json" \
  -d '{"expression": "document.title"}'
```

## Next Steps

- [Architecture](./architecture.md) — Understand how Victauri works under the hood
- [Tools Reference](./tools-reference.md) — Complete list of all 33 tools
- [Configuration](./configuration.md) — Customize port, auth, privacy, and more
- [Testing](./testing.md) — Write automated tests with the victauri-test crate
