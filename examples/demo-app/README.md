# Victauri Demo App

Minimal Tauri app instrumented with Victauri for testing demos.

All 12 commands use `#[inspectable]` and are registered in the Victauri command registry, making them discoverable via MCP.

## Commands

| Category | Command | Description |
|----------|---------|-------------|
| general | `greet` | Returns a greeting for the given name |
| counter | `get_counter` | Reads the current counter value |
| counter | `increment` | Increments the counter by 1 |
| counter | `decrement` | Decrements the counter by 1 |
| counter | `reset_counter` | Resets the counter to zero |
| todos | `add_todo` | Creates a new todo item |
| todos | `list_todos` | Lists all todo items |
| todos | `toggle_todo` | Toggles a todo's completion status |
| todos | `delete_todo` | Deletes a todo by ID |
| settings | `get_settings` | Returns current app settings |
| settings | `update_settings` | Updates theme, notifications, or language |
| debug | `get_app_state` | Dumps full application state as JSON |

## Run

```bash
cd examples/demo-app
cargo tauri dev
```

## Test

With the app running:
```bash
cargo test -p demo-app --test integration
```

Or connect an AI agent via MCP at `http://127.0.0.1:7373/mcp`.

## MCP Configuration

The included `.mcp.json` allows Claude Code to connect immediately:

```json
{
  "mcpServers": {
    "victauri-demo": {
      "url": "http://127.0.0.1:7373/mcp"
    }
  }
}
```
