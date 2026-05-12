# Victauri for VS Code

Full-stack inspection of running Tauri apps, directly in your editor.

See windows, DOM, IPC traffic, memory usage, and diagnostics — all live. Generate tests from the UI. Take screenshots. Evaluate JS. No browser DevTools needed.

## Requirements

- A Tauri 2.x app running with [victauri-plugin](https://crates.io/crates/victauri-plugin) enabled
- The plugin starts an HTTP server on `127.0.0.1:7373` that this extension connects to

## Features

### Activity Bar

Three tree views in the Victauri sidebar:

- **App State** — windows (size, visibility, URL), memory usage, performance metrics (JS heap, DOM stats, long tasks), plugin version/uptime, diagnostic warnings
- **DOM Explorer** — full accessible tree with ref IDs, ARIA roles, element bounds. Right-click to click, highlight, inspect styles, copy ref IDs, or generate test code
- **IPC Log** — live Tauri command log with status codes, duration, timestamps

### Screenshot Panel

`Victauri: Take Screenshot` opens an inline webview panel with the current app screenshot. Refresh or save to disk without leaving VS Code.

### CodeLens

Every `#[tauri::command]` function in Rust files gets a "Generate Victauri test" lens. Click it to scaffold an `e2e_test!` block that invokes the command and verifies the result.

### DOM Interactions

Right-click any element in the DOM Explorer to:

- **Click Element** — trigger a click in the running app
- **Highlight Element** — draw a colored overlay on the element
- **Inspect Styles** — dump computed CSS to the Output panel
- **Copy Ref ID** — copy the element's ref handle
- **Generate Test** — scaffold a test for the element

### Accessibility Audit

`Victauri: Audit Accessibility` runs a comprehensive WCAG check — missing alt text, unlabeled inputs, heading hierarchy, color contrast, ARIA validity — and reports violations in the Output panel.

### Performance Metrics

`Victauri: Performance Metrics` shows navigation timing, JS heap usage, DOM stats, long task count, and resource loading details. The App State tree also shows a live performance summary.

### Diagnostics

`Victauri: Run Diagnostics` checks for compatibility issues — CSP problems, missing bridge methods, shadow DOM, service workers — and reports them in the Output panel.

### Status Bar

Shows connection state at a glance. Click to connect or disconnect.

## Commands

| Command | Description |
|---|---|
| `Victauri: Connect to Tauri App` | Connect to a running Victauri server |
| `Victauri: Disconnect` | Disconnect from the server |
| `Victauri: Take Screenshot` | Capture the app window |
| `Victauri: Evaluate JavaScript` | Run JS in the Tauri webview |
| `Victauri: Run Diagnostics` | Check for compatibility issues |
| `Victauri: Audit Accessibility` | Run WCAG accessibility audit |
| `Victauri: Performance Metrics` | Show navigation timing, heap, DOM stats |
| `Victauri: Clear Highlights` | Remove all debug overlays from the app |
| `Victauri: Refresh All Views` | Force-refresh all tree views |

## Configuration

| Setting | Default | Description |
|---|---|---|
| `victauri.port` | `7373` | Port of the Victauri server |
| `victauri.autoConnect` | `true` | Auto-connect when a Tauri project is detected in the workspace |
| `victauri.pollInterval` | `2000` | Polling interval (ms) for live updates |
| `victauri.authToken` | `""` | Bearer token for authenticated connections |

## How It Works

The extension talks to Victauri's REST API (`/api/tools/{name}`), which runs inside your Tauri app's process. This gives it access to the webview DOM, Rust backend state, IPC traffic, and native window state simultaneously — something external tools like Playwright can't do.

Auto-connect detects `tauri.conf.json` in your workspace and connects on activation. Port discovery reads `victauri.port` from the temp directory if the default port is taken.

## Install from Source

```bash
cd editors/vscode
npm install
npm run build
```

Then in VS Code: `Ctrl+Shift+P` > `Developer: Install Extension from Location...` > select the `editors/vscode` directory.

## License

Apache-2.0
