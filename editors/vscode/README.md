# Victauri for VS Code

[![Version](https://img.shields.io/visual-studio-marketplace/v/4da-systems.victauri)](https://marketplace.visualstudio.com/items?itemName=4da-systems.victauri)
[![Installs](https://img.shields.io/visual-studio-marketplace/i/4da-systems.victauri)](https://marketplace.visualstudio.com/items?itemName=4da-systems.victauri)
[![License](https://img.shields.io/github/license/4DA-Systems/victauri)](https://github.com/4DA-Systems/victauri/blob/main/LICENSE)

**X-ray vision for Tauri apps.** See your app's DOM tree, IPC traffic, window state, memory usage, and performance metrics — all live inside VS Code. Click elements, take screenshots, run accessibility audits, and generate tests without leaving your editor.

> Works with any Tauri 2.x app. Framework-agnostic — tested with React, Vue, Svelte, and vanilla JS.


## Quick Start

1. Add `victauri-plugin` to your Tauri app ([one-line setup](https://github.com/4DA-Systems/victauri#quick-start))
2. Run your app in debug mode
3. The extension auto-connects when it detects `tauri.conf.json` in your workspace

That's it. The sidebar populates with live app state, the DOM tree, and IPC traffic.

## Features

### Activity Bar — Three Live Views

| View | What You See |
|---|---|
| **App State** | Windows (size, visibility, URL), memory usage, JS heap, DOM stats, long tasks, plugin version, diagnostic warnings |
| **DOM Explorer** | Full accessible tree with ref IDs, ARIA roles, element bounds. Right-click to interact, inspect, or generate tests |
| **IPC Log** | Live Tauri command log with status codes, duration, timestamps |

### Screenshot Panel

`Victauri: Take Screenshot` captures the app window into an inline webview panel. Refresh or save to disk without leaving VS Code.

### CodeLens — Test Generation

Every `#[tauri::command]` function in Rust files gets a "Generate Victauri test" lens. Click it to scaffold an `e2e_test!` block that invokes the command and verifies the result.

### DOM Interactions

Right-click any element in the DOM Explorer:

- **Click Element** — trigger a click in the running app
- **Highlight Element** — draw a colored overlay on the element
- **Inspect Styles** — dump computed CSS to the Output panel
- **Copy Ref ID** — copy the element's ref handle
- **Generate Test** — scaffold a test for the element

### Accessibility Audit

`Victauri: Audit Accessibility` runs a comprehensive WCAG check — missing alt text, unlabeled inputs, heading hierarchy, color contrast, ARIA validity — and reports violations in the Output panel.

### Performance Metrics

`Victauri: Performance Metrics` shows navigation timing, JS heap usage, DOM stats, long task count, and resource loading details.

### Diagnostics

`Victauri: Run Diagnostics` checks for compatibility issues — CSP problems, missing bridge methods, shadow DOM, service workers.

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
| `victauri.autoConnect` | `true` | Auto-connect when a Tauri project is detected |
| `victauri.pollInterval` | `2000` | Polling interval (ms) for live updates |
| `victauri.authToken` | `""` | Bearer token for authenticated connections |

## How It Works

Victauri runs an HTTP server **inside your Tauri app's process**. This extension talks to its REST API, giving it simultaneous access to the webview DOM, Rust backend state, IPC traffic, and native window state — something external tools like Playwright can't do.

The plugin is gated behind `#[cfg(debug_assertions)]`, so `init()` is a no-op in release builds — zero runtime cost (the server never starts). Add it as a normal dependency (the app binary cannot use `[dev-dependencies]`); to also keep it out of the release binary, make it an optional dependency behind a Cargo feature and gate the `.plugin(...)` call on that feature.

## Requirements

- A Tauri 2.x app with [victauri-plugin](https://crates.io/crates/victauri-plugin) enabled
- The plugin starts an HTTP server on `127.0.0.1:7373` that this extension connects to
- Port discovery reads the per-process discovery directory (`%TEMP%\victauri\<pid>\` on Windows; `$XDG_RUNTIME_DIR/victauri/<pid>/` or `<temp>/victauri-<uid>/<pid>/` on Unix, plus the legacy `<temp>/victauri/`) — trusted entries owned by a live process of yours only — if the default port is taken. On Windows a discovery directory outside your user profile (a shared `TEMP`, e.g. MSYS2's `C:\msys64\tmp`) is used only if you own it, checked with PowerShell's `Get-Acl`; where PowerShell cannot run, set `victauri.port` and `victauri.authToken` instead

## Compatibility

In a one-time evaluation (May 2026, on an older Victauri release) against 5 real-world open-source Tauri apps, 867/895 checks passed (96.9%). Results of that evaluation (the per-release harness in `scripts/compat` re-verifies Kanri, En Croissant and Lettura on each release):

| App | Framework | Result |
|---|---|---|
| Kanri | Vue 3 / Nuxt | 174/179 |
| En Croissant | React / Mantine | 177/179 |
| Surrealist | React 19 / Mantine | 176/179 |
| Duckling | React 19 / Jotai | 169/179 |
| Lettura | React / Custom UI | 171/179 |

## License

Apache-2.0
