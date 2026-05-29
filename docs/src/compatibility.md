# Tauri App Compatibility

Victauri works with any Tauri 2.x application. This page documents compatibility considerations discovered through research against real-world open-source Tauri apps and platform-level investigation.

## Content Security Policy (CSP)

**Short answer: CSP does not block Victauri on any platform.**

Victauri's JS bridge is injected via Tauri's `js_init_script()` API, and all tool calls use Tauri's `webview.eval()` which delegates to native platform APIs:

| Platform | Native API | CSP bypass |
|---|---|---|
| Windows | `ICoreWebView2.ExecuteScriptAsync()` | Yes (Chromium CDP `allowUnsafeEvalBlockedByCSP` defaults to true) |
| macOS | `WKWebView.evaluateJavaScript()` | Yes (privileged bridge execution) |
| Linux | `webkit_web_view_run_javascript()` | Yes ([WebKitGTK docs confirm explicit bypass](https://webkitgtk.org/reference/webkit2gtk/stable/property.WebView.default-content-security-policy.html)) |

The bridge code itself never uses `eval()`, `new Function()`, `setTimeout(string)`, or any other CSP-sensitive pattern. All JavaScript is direct DOM API calls and function closures.

**Why this works:** Native webview eval is a host-application privilege, not a page-level script execution. It operates outside the web security model, similar to how browser DevTools can evaluate code regardless of CSP.

Even apps with strict CSP like `"script-src 'self'"` (Spacedrive) should work with Victauri. Use `get_diagnostics` to verify.

## Known Edge Cases

### Shadow DOM (closed mode)

Victauri traverses **open** shadow roots automatically (`element.shadowRoot` returns the shadow tree). However, **closed shadow roots** (`{ mode: "closed" }`) return `null` — their contents are invisible to `dom_snapshot`, `find_elements`, and `audit_accessibility`.

**Affected components:** Shoelace, some Ionic components. Lit and Stencil default to open mode.

**Detection:** `get_diagnostics` reports custom elements that may have closed shadow roots.

**Workaround:** None possible — this is a browser security boundary. If you control the component, switch to `mode: "open"` in debug builds.

### iframes

Tauri's `js_init_script` does **not** run inside iframes ([tauri-apps/tauri#13577](https://github.com/tauri-apps/tauri/issues/13577)). The Victauri bridge will be absent inside any `<iframe>` element. Tools like `dom_snapshot` will show the `<iframe>` element but not its contents.

**Detection:** `get_diagnostics` reports iframe count and sources.

**Workaround:** None — this is a Tauri limitation. Tauri recommends using multi-webview (`WebviewWindow`) instead of iframes for security.

### Service Workers

Service workers can intercept `fetch()` calls, including calls to `http://ipc.localhost/` which Victauri uses to capture IPC traffic. An active service worker may cause:
- Missing entries in `get_ipc_log`
- False negatives in `detect_ghost_commands` and `check_ipc_integrity`

Additionally, [tauri-apps/tauri#12673](https://github.com/tauri-apps/tauri/issues/12673) documents that service workers break `invoke()` and `emit()` on second app launch.

**Detection:** `get_diagnostics` warns when `navigator.serviceWorker.controller` is active.

**Risk level:** Low — service workers require `https://` or `http://localhost` origin, so they only affect apps using `tauri-plugin-localhost`. Most Tauri apps use the `tauri://` protocol which doesn't support service workers.

### Alternative IPC Transports

#### rspc / tauri-specta

Apps like Spacedrive route all RPC through a single Tauri command (e.g., `daemon_request`). Victauri's IPC log will show the wrapper command but not the inner procedure names. `get_registry` returns empty unless commands are also registered with `#[inspectable]`.

#### tauri-invoke-http

[tauri-invoke-http](https://github.com/tauri-apps/tauri-invoke-http) replaces Tauri's IPC transport entirely with a localhost HTTP server, bypassing `ipc.localhost`. Victauri's IPC interception will miss all calls. This plugin is very niche.

#### Sidecar processes

Apps using Node.js sidecars (Yaak) or daemon processes (Spacedrive) for backend logic — Victauri only sees the Tauri IPC boundary. Sidecar/gRPC traffic is invisible.

### Large DOM

Victauri walks the entire DOM tree for `dom_snapshot`. Performance scales linearly:

| Elements | Approximate time |
|---|---|
| 1,000 | ~10ms |
| 5,000 | ~50ms |
| 10,000 | ~100ms |
| 50,000 | ~500ms |

Apps using virtualized lists (react-window, tanstack-virtual) only render visible items, so the actual DOM count is smaller than the data set.

**Detection:** `get_diagnostics` warns when DOM exceeds 5,000 elements.

### Plugin Port Conflicts

`tauri-plugin-localhost` also binds a localhost HTTP server. Victauri's port fallback (7373 → 7374 → ... → 7383) and automatic port file discovery handle this. No action needed.

### Custom Invoke Handlers

Tauri 2 supports only one `invoke_handler` per app ([tauri-apps/tauri#11447](https://github.com/tauri-apps/tauri/issues/11447)). Victauri intercepts IPC via fetch monitoring, not by wrapping the invoke handler, so custom handlers do not affect Victauri.

### Platform-Specific Notes

| Platform | Requirement | Notes |
|---|---|---|
| Windows | WebView2 runtime | Pre-installed on Windows 11; auto-installed on Windows 10. Evergreen (auto-updates). |
| macOS | macOS 10.15+ | WKWebView ships with the OS. No additional runtime. |
| Linux | WebKitGTK 2.36+ (webkit2gtk-4.1) | Ubuntu 22.04+. Tauri 2 won't compile on older versions, so not a Victauri concern. |

### Tauri Version Compatibility

Victauri is tested with Tauri 2.0 through 2.11. Key compatibility facts:
- `js_init_script()` API is stable across all 2.x releases
- The `fetch()` → `ipc.localhost` IPC transport is unchanged since Tauri 2.0
- Plugin init scripts run in IIFE isolation since April 2024 — no global scope conflicts
- The `plugin:victauri|` namespace prefix is automatically excluded from IPC logs

## Diagnostics Tool

Call `get_diagnostics` (MCP or REST) to check for all known edge cases at runtime:

```bash
curl -X POST http://127.0.0.1:7373/api/tools/get_diagnostics -d '{}'
```

Returns:
```json
{
  "result": {
    "warnings": [
      {
        "id": "closed-shadow-dom",
        "severity": "medium",
        "message": "3 custom element(s) may use closed shadow DOM",
        "details": { "count": 3 }
      }
    ],
    "info": {
      "bridge_version": "0.6.0",
      "dom_elements": 847,
      "open_shadow_roots": 12,
      "event_listeners": 234,
      "protocol": "tauri:",
      "url": "tauri://localhost/",
      "user_agent": "..."
    }
  }
}
```

Warning IDs: `service-worker-active`, `closed-shadow-dom`, `iframes-present`, `large-dom`.

## Multi-Window Apps

Victauri handles multi-window apps automatically. Default window selection: `"main"` → first visible → any. Use `webview_label` to target specific windows:

```rust
client.eval_js_in("settings", "document.title").await?;
client.dom_snapshot_for("notification").await?;
```

Apps with many dynamic windows (Spacedrive's 15+ types, Seelen-UI's desktop environment) should target windows by label explicitly.

## Tested Apps

| App | Stars | Tauri | Frontend | Windows | Commands | Victauri Fit |
|---|---|---|---|---|---|---|
| [Vibe](https://github.com/thewh1teagle/vibe) | 6.1k | 2.x | TypeScript | 1 | 41 | Excellent |
| [Bokuchi](https://github.com/Bokuchi-Editor/bokuchi) | 68 | 2.x | React | 1 | 15 | Excellent |
| [Yaak](https://github.com/mountain-loop/yaak) | 18.6k | 2.11 | React 19 | 1 | ~30-60 | Good (gRPC sidecar invisible) |
| [Whispering](https://github.com/EpicenterHQ/epicenter) | 4.5k | 2.x | Svelte 5 | 1 | ~10-20 | Good (framework diversity) |
| [Wealthfolio](https://github.com/wealthfolio/wealthfolio) | 7.4k | 2.10 | React | 1 | Standard | Good (single-instance plugin) |
| [Clash Nyanpasu](https://github.com/libnyanpasu/clash-nyanpasu) | 13k | 2.4 | React 19 | Dynamic | Standard | Good (dynamic windows, specta IPC) |
| [Spacedrive](https://github.com/spacedriveapp/spacedrive) | 38k | 2.1 | React + rspc | 15+ | rspc-routed | Partial (rspc opaque, multi-window) |
| [Seelen-UI](https://github.com/eythaann/Seelen-UI) | 16.8k | 2.10 | Svelte 5 | Many | Standard | Hard (desktop environment) |
| [Hoppscotch Agent](https://github.com/hoppscotch/hoppscotch) | 79k | 2.9 | Minimal | 1 | 2 | Too minimal |

## Framework Coverage

Victauri's DOM snapshot uses the accessible tree (ARIA roles and names), which is framework-agnostic. Confirmed working with:

- React (demo-app, 4DA)
- Vue (Hoppscotch web client)
- Svelte (Whispering, Seelen-UI)
- Vanilla HTML/JS
