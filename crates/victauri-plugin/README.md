# victauri-plugin

The main [Victauri](https://github.com/runyourempire/victauri) crate -- an embedded MCP server that gives AI agents full-stack control of Tauri 2.0 applications.

## Quick Start

Add the dependency:

```toml
[dependencies]
victauri-plugin = "0.5"
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

Connect Claude Code (or any MCP client) via the `victauri bridge` stdio proxy — it discovers
the running app's port automatically and survives restarts:

```json
{ "mcpServers": { "victauri": { "command": "victauri", "args": ["bridge", "--wait"] } } }
```

(The raw endpoint is `http://127.0.0.1:7373/mcp`, but a fixed `url:` hardcodes a port and can
bind the wrong app — prefer the bridge.)

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

31 MCP tools across three layers -- webview, IPC, and Rust backend:

### Backend (direct Rust access, no webview needed)

| Tool | What it does |
|---|---|
| `app_info` | App config, directory paths, env vars, discovered databases, process info |
| `list_app_dir` | List files in app data/config/log/local_data directories |
| `read_app_file` | Read files from app backend directories (UTF-8 or base64) |
| `query_db` | Read-only SQLite queries with auto-discovery |
| `get_memory_stats` | Real-time OS process memory (working set, page faults) |
| `invoke_command` | Call any Tauri command directly through IPC |

### IPC Layer

| Tool | What it does |
|---|---|
| `get_registry` | List all commands with schemas from `#[inspectable]` |
| `detect_ghost_commands` | Find frontend IPC calls with no backend handler |
| `check_ipc_integrity` | Detect stuck, stale, or errored IPC calls |
| `verify_state` | Compare frontend DOM against backend state |
| `resolve_command` | Natural language to matching Tauri command |

### Webview (DOM, interactions, JS)

| Tool | What it does |
|---|---|
| **`interact`** | Click, double-click, hover, focus, scroll, select |
| **`input`** | Fill inputs, type character-by-character, press keyboard keys |
| **`inspect`** | Computed CSS, bounding boxes, element highlighting, a11y audit, performance metrics |
| **`css`** | Inject/remove debug CSS |
| `eval_js` | Execute JavaScript in the webview |
| `dom_snapshot` | Full accessibility tree with ref handles |
| `find_elements` | Search for elements by text, role, test ID, or CSS selector |
| `screenshot` | Platform-native window capture |
| `assert_semantic` | Evaluate JS expression and assert against expected value |
| `wait_for` | Poll for conditions: text appears, selector matches, IPC settles |

### App-wide

| Tool | What it does |
|---|---|
| **`window`** | Get state, list windows, manage, resize, move, set title |
| **`storage`** | Read/write localStorage, sessionStorage, cookies |
| **`navigate`** | Go to URL, go back, get history, configure dialog responses |
| **`recording`** | Start/stop sessions, checkpoints, get events, export/import |
| **`logs`** | Console, network, IPC, navigation, dialog logs |
| **`introspect`** | Command timings, coverage, contract testing, startup timing, capabilities, DB health, managed state, tasks |
| **`fault`** | Inject IPC faults: delay, error, drop, corrupt (chaos engineering) |
| **`explain`** | Natural-language narration: summary, last action, diff |
| `get_plugin_info` | Victauri config: port, enabled tools, version |
| `get_diagnostics` | Server health, compatibility warnings, tool status |

## Security

- **Localhost only** -- binds to `127.0.0.1`, never exposed to network
- **Debug-only** -- `init()` returns a no-op plugin in release builds
- **Auth on by default** -- auto-generated Bearer token (auto-discovered by clients); fixed token via builder or `VICTAURI_AUTH_TOKEN`; `.auth_disabled()` to opt out
- **Rate limiting** -- Token-bucket at 1000 req/sec (default)
- **Privacy layer** -- Command allowlists/blocklists, tool disabling, regex redaction

See the full [project README](https://github.com/runyourempire/victauri) for architecture details and live test results.

## Documentation

Full API docs: [docs.rs/victauri-plugin](https://docs.rs/victauri-plugin)

## License

Apache-2.0 -- see [LICENSE](../../LICENSE)

Part of [Victauri](https://github.com/runyourempire/victauri). Built by [4DA Systems](https://4da.ai).
