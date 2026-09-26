# victauri-watchdog

Lightweight crash-recovery sidecar for the [Victauri](https://github.com/4DA-Systems/victauri) MCP server.

Since Victauri's MCP server runs inside the Tauri app process, a crash kills the server too. The watchdog runs as a separate process, detects failures, and can trigger recovery.

## What It Does

- Discovers the app's port from `<temp>/victauri/<pid>/port` (live processes only; the plugin
  may bind 7374+ when 7373 is taken), optionally selecting an app by identity with
  `--app <identifier>` / `VICTAURI_APP`, and follows the app if it restarts on a new port.
  An explicit `VICTAURI_PORT` (or positional `PORT`) skips discovery entirely.
- Polls `GET /health` on the Victauri MCP server at a configurable interval
- Logs warnings on first failure, errors after consecutive misses
- Executes a configurable recovery command after threshold failures
- Resets failure count automatically when the server recovers

## Installation

```bash
cargo install victauri-watchdog
```

## Usage

```bash
# Default: discover the running app's port (fallback 7373), poll every 5 seconds
victauri-watchdog

# Several Victauri apps running? Pick one by bundle identifier (or product name)
victauri-watchdog --app com.your.app

# Custom port and interval
VICTAURI_PORT=8080 VICTAURI_INTERVAL=10 victauri-watchdog

# With recovery command (runs after 3 consecutive failures)
VICTAURI_ON_FAILURE="systemctl restart my-tauri-app" victauri-watchdog
```

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `VICTAURI_PORT` | _(discovered, else `7373`)_ | Port to poll; setting it skips discovery |
| `VICTAURI_APP` | _(none)_ | Discovery selector: app bundle identifier or product name (same as `--app`) |
| `VICTAURI_INTERVAL` | `5` | Seconds between health checks |
| `VICTAURI_MAX_FAILURES` | `3` | Consecutive failures before recovery action |
| `VICTAURI_ON_FAILURE` | _(none)_ | Shell command to execute on failure threshold |

The recovery command fires once per failure cycle. If the server comes back, the counter resets.

## Documentation

Full API docs: [docs.rs/victauri-watchdog](https://docs.rs/victauri-watchdog)

## License

Apache-2.0 -- see [LICENSE](../../LICENSE)

Part of [Victauri](https://github.com/4DA-Systems/victauri). Built by [4DA Systems](https://4da.ai).
