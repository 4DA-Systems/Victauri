# victauri-plugin

The main [Victauri](https://github.com/runyourempire/victauri) crate -- an embedded MCP server that gives AI agents full-stack control of Tauri 2.0 applications.

## Quick Start

Add the dependency (dev-only):

```toml
[dev-dependencies]
victauri-plugin = "0.2"
```

Wire it into your Tauri app:

```rust
fn main() {
    tauri::Builder::default()
        .plugin(victauri_plugin::init())
        .run(tauri::generate_context!())
        .expect("error running app");
}
```

Connect Claude Code (or any MCP client) to `http://127.0.0.1:7373/mcp`.

## Configuration

Use `VictauriBuilder` for advanced setup:

```rust,ignore
victauri_plugin::VictauriBuilder::new()
    .port(8080)
    .auth_token("my-secret-token")
    .disable_tools(&["screenshot"])
    .add_redaction_pattern(r"\b\d{3}-\d{2}-\d{4}\b")
    .enable_redaction()
    .build()
    .expect("valid config")
```

## Tools

24 MCP tools -- 9 compound tools (each with multiple actions) and 15 standalone:

| Tool | What it does |
|---|---|
| **`interact`** | Click, double-click, hover, focus, scroll, select |
| **`input`** | Fill inputs, type character-by-character, press keyboard keys |
| **`window`** | Get state, list windows, manage, resize, move, set title |
| **`storage`** | Read/write localStorage, sessionStorage, cookies |
| **`navigate`** | Go to URL, go back, get history, configure dialog responses |
| **`recording`** | Start/stop sessions, checkpoints, get events, export/import |
| **`inspect`** | Computed CSS, bounding boxes, element highlighting, a11y audit, performance metrics |
| **`css`** | Inject/remove debug CSS |
| **`logs`** | Console, network, IPC, navigation, dialog logs |
| `eval_js` | Execute JavaScript in the webview |
| `dom_snapshot` | Full accessibility tree with ref handles |
| `find_elements` | Search for elements by text, role, test ID, or CSS selector |
| `invoke_command` | Call any registered Tauri command through real IPC |
| `screenshot` | Platform-native window capture |
| `verify_state` | Compare frontend DOM state against backend state |
| `detect_ghost_commands` | Find frontend IPC calls with no backend handler |
| `check_ipc_integrity` | Detect stuck, stale, or errored IPC calls |
| `wait_for` | Poll for conditions: text appears, selector matches, IPC settles |
| `assert_semantic` | Evaluate JS expression and assert against expected value |
| `resolve_command` | Natural language to matching Tauri command |
| `get_registry` | List all commands with schemas from `#[inspectable]` |
| `get_memory_stats` | Real-time process memory statistics |
| `get_plugin_info` | Victauri config: port, enabled tools, version |
| `get_diagnostics` | Server health, compatibility warnings, tool status |

## Security

- **Localhost only** -- binds to `127.0.0.1`, never exposed to network
- **Debug-only** -- `init()` returns a no-op plugin in release builds
- **Optional auth** -- Bearer token via builder or `VICTAURI_AUTH_TOKEN` env var
- **Rate limiting** -- Token-bucket at 1000 req/sec (default)
- **Privacy layer** -- Command allowlists/blocklists, tool disabling, regex redaction

See the full [project README](https://github.com/runyourempire/victauri) for architecture details and live test results.

## Documentation

Full API docs: [docs.rs/victauri-plugin](https://docs.rs/victauri-plugin)

## License

Apache-2.0 -- see [LICENSE](../../LICENSE)

Part of [Victauri](https://github.com/runyourempire/victauri). Built by [4DA Systems](https://4da.ai).
