# Architecture

Victauri embeds a full MCP server inside your running Tauri application. This page explains the design decisions and how the pieces fit together.

## The Three Layers

Victauri provides access to three distinct layers of a Tauri application:

```
┌─────────────────────────────────────────────────┐
│                   MCP Client                     │
│          (Claude Code, VS Code, etc.)            │
└─────────────────┬───────────────────────────────┘
                  │ HTTP/SSE (localhost:7373)
┌─────────────────▼───────────────────────────────┐
│              Victauri Plugin                      │
│         (axum server + tool handlers)            │
├─────────────────┬────────────┬──────────────────┤
│    WebView      │    IPC     │    Backend        │
│   (JS Bridge)   │  (Intercept)│  (AppHandle)     │
└────────┬────────┴─────┬──────┴────────┬─────────┘
         │              │               │
    DOM/Events     Command Flow    Rust State
```

### WebView Layer

The JS bridge is injected into every webview via `js_init_script()` (persistent across navigations). It provides:

- **DOM snapshots** — Full accessible tree with ARIA roles, names, and ref handles
- **Element interaction** — Click, hover, fill, type, press keys with Playwright-grade actionability checks
- **JS evaluation** — Run arbitrary JavaScript with async/await support
- **CSS inspection** — Computed styles, bounding boxes with box model
- **Console/mutation logs** — Captured in-bridge with configurable capacity
- **Network interception** — Fetch and XMLHttpRequest monitoring
- **Navigation tracking** — pushState, replaceState, popstate, hashchange

### IPC Layer

Tauri 2.0 sends all IPC via `fetch()` to `http://ipc.localhost/<command>`. Victauri intercepts this at the network level:

- **Command registry** — Discover all available commands with metadata
- **IPC log** — Full history of command invocations with timing
- **Ghost command detection** — Find frontend-invoked commands not in the registry
- **Integrity checking** — Detect stale, pending, or errored IPC calls

### Backend Layer

Since the plugin runs in the same process, it has direct access to:

- **AppHandle** — Manage windows, invoke commands, read state
- **Memory stats** — Real OS process memory (working set, page faults)
- **Diagnostics** — Plugin uptime, tool invocation counts, configuration

## Same-Process Embedded Design

Unlike external automation tools that communicate over DevTools Protocol or WebSocket bridges, Victauri runs **inside** the application process:

```
External approach:          Victauri approach:
                            
Agent ──HTTP──► Proxy       Agent ──HTTP──► Tauri App
                 │                          (contains MCP server)
                CDP                         Direct AppHandle access
                 │                          Sub-ms response times
               Browser                      No state drift
```

Benefits:
- **No state drift** — The MCP server reads the same memory as the application
- **Sub-millisecond responses** — No IPC hop to an external process
- **Full access** — Can read Rust state, invoke commands, access the database directly
- **Single dependency** — No separate process to manage or keep alive

## The JS Bridge

The bridge (`window.__VICTAURI__`) is injected as an init script so it survives page navigations:

```javascript
// Available methods on window.__VICTAURI__:
__VICTAURI__.version          // Bridge version string
__VICTAURI__.snapshot()       // Full DOM tree with refs
__VICTAURI__.getRef(id)       // Get element by ref handle
__VICTAURI__.click(ref)       // Click with actionability checks
__VICTAURI__.fill(ref, val)   // Set input value
__VICTAURI__.type(ref, text)  // Type character-by-character
__VICTAURI__.pressKey(key)    // Dispatch keyboard event
__VICTAURI__.getConsoleLogs() // Captured console entries
__VICTAURI__.getStyles(ref)   // Computed CSS properties
// ... 20+ methods total
```

### Ref Handles

Following Playwright MCP's pattern, elements are identified by short-lived **ref handles** rather than CSS selectors:

- Refs are derived from the accessible tree (ARIA roles and names)
- They are short strings like `"e3"` or `"e47"`
- They survive DOM restructuring within a single snapshot
- A new `dom_snapshot` generates fresh refs

This avoids brittle CSS selectors and gives agents a semantic view of the UI.

### Actionability Checks

Before interactions (click, fill, type, hover), the bridge performs Playwright-grade checks:

1. Element exists in DOM
2. Element is visible (not `display:none` or `visibility:hidden`)
3. Element is enabled (not `disabled` attribute)
4. Element has non-zero size
5. Element is not covered by another element (hit-test)
6. Element does not have `pointer-events:none`
7. Element is in viewport (with auto-scroll)
8. Element is stable (not animating)
9. Element is attached to DOM
10. Element is actionable for the specific operation

## Dual Protocol: MCP + REST

Victauri serves both protocols on the same port:

| Endpoint | Protocol | Use Case |
|----------|----------|----------|
| `/mcp` | MCP Streamable HTTP + SSE | AI agents (Claude Code, etc.) |
| `/api/tools` | REST (plain JSON) | Scripts, CI, curl, custom integrations |
| `/health` | GET (no auth) | Health checks, watchdog |
| `/info` | GET | Server metadata |

The REST API uses the same tool dispatch, auth, rate limiting, and privacy enforcement as MCP. It simply removes the session/handshake overhead.

### MCP Resources

Three subscribable resources provide real-time state:

- `victauri://state` — Plugin state (commands registered, events captured, memory, port)
- `victauri://windows` — All window states (position, size, visibility, URL)
- `victauri://ipc-log` — Recent IPC call history

## Port Fallback

If port 7373 is already in use (e.g., another Tauri app running Victauri), the server tries ports 7374 through 7383. The actual bound port is written to a temp file (`<temp>/victauri.port`) for client discovery and cleaned up on shutdown.

## Release Safety

The entire plugin is gated:

```rust
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    #[cfg(debug_assertions)]
    { /* full MCP server setup */ }
    
    #[cfg(not(debug_assertions))]
    { /* no-op plugin with zero overhead */ }
}
```

In release builds:
- No axum server is started
- No JS bridge is injected
- No memory is allocated for event logs
- The binary size increase is zero
