# FAQ

## General

### What Tauri versions are supported?

Victauri supports Tauri 2.0 and later. It is not compatible with Tauri 1.x due to fundamental differences in the plugin system and IPC architecture.

### Does Victauri work with any frontend framework?

Yes. Victauri is framework-agnostic. It has been tested with:
- React (18, 19)
- Vue 3 / Nuxt
- Svelte / SvelteKit
- Any framework that renders to the DOM

The JS bridge operates at the DOM level and does not depend on any framework internals.

### Can I use Victauri in production?

No, by design. The entire plugin is gated behind `#[cfg(debug_assertions)]` and compiles to a no-op in release builds. This is intentional — Victauri provides full introspection and control capabilities that should never be available in production.

### Is there any performance impact?

In **debug builds**: The JS bridge adds a small overhead for network/console/navigation interception (hooks into `fetch`, `console.*`, and history APIs). The MCP server itself uses negligible resources when idle.

In **release builds**: Zero overhead. The plugin is completely compiled out.

### How is this different from Playwright?

Playwright sees only the browser glass (DOM). Victauri gives agents simultaneous access to:
- The DOM (like Playwright)
- The Rust backend state
- The IPC layer between frontend and backend
- Native window state
- Process memory and diagnostics

Playwright requires an external process and CDP. Victauri runs inside the app with direct access.

### How is this different from Tauri's built-in testing?

Tauri's testing utilities (`tauri-driver`, WebDriver) focus on end-to-end automation. Victauri provides:
- MCP protocol for AI agent integration
- Cross-boundary state verification (frontend vs backend)
- Time-travel recording and replay
- Ghost command detection
- Accessibility auditing
- All accessible through a standard protocol any MCP client can use

## Setup

### Why do I need `victauri:default` in capabilities?

Tauri 2.0's permission system blocks IPC calls that don't have matching capability grants. Without `victauri:default`, the plugin's webview callbacks are silently dropped by Tauri's security layer — no error is shown, things just don't work.

### The MCP server doesn't start — what's wrong?

Check that:
1. You're running a **debug** build (`cargo run`, not `cargo run --release`)
2. The port isn't already in use (check the logs for port fallback messages)
3. The plugin is initialized before `.run()`: `.plugin(victauri_plugin::init())`

### How do I find the actual port?

If the default port (7373) is busy, Victauri tries 7374-7383. The actual port is:
- Printed to stdout/logs on startup
- Written to `<temp_dir>/victauri.port`
- Available via `GET /info` on the bound port
- Discoverable by the `victauri check` CLI command

### My frontend uses CSP — will eval work?

Yes. The JS bridge uses init scripts (injected before page load) and direct function invocation patterns that work within standard CSP policies. The `eval_js` tool evaluates code through the Tauri webview's `eval()` mechanism, which operates outside the page's CSP sandbox.

## Tools

### Why do refs change between snapshots?

Refs are short-lived handles tied to the DOM state at snapshot time. If the DOM changes (user interaction, framework re-render, dynamic content), refs from a previous snapshot may no longer be valid. Always take a fresh `dom_snapshot` before interacting with elements.

### Why does click/fill fail with "element not actionable"?

Victauri performs Playwright-grade actionability checks before interactions:
- Element must be visible (`display` not `none`, `visibility` not `hidden`)
- Element must be enabled (no `disabled` attribute)
- Element must have non-zero size
- Element must not be covered by another element (overlays, modals)
- Element must not have `pointer-events: none`

This prevents flaky tests that interact with hidden or covered elements. If you're seeing this error, check your UI state — another element (modal backdrop, loading overlay, tooltip) may be covering the target.

### How does `invoke_command` work?

It calls `window.__TAURI_INTERNALS__.invoke(command, args)` in the webview, which triggers the standard Tauri IPC flow. The command must be registered in your `invoke_handler`. If the command requires specific Tauri permissions/capabilities, those must also be configured.

### What's the eval timeout?

Default: 30 seconds. This is long enough to support `wait_for` polling and async operations. Configurable via `VictauriBuilder::eval_timeout()` up to 300 seconds.

### Can I invoke commands with complex arguments?

Yes. Pass arguments as a JSON object:

```json
{
  "command": "create_todo",
  "args": {
    "title": "Buy groceries",
    "priority": 3,
    "tags": ["shopping", "urgent"]
  }
}
```

## Architecture

### Why not use Chrome DevTools Protocol?

CDP requires an external debugger connection and only works with Chromium-based webviews. Victauri's embedded approach:
- Works on all platforms identically (no CDP dependency)
- Has access to the Rust backend (CDP can't see that)
- Doesn't require debug flags or remote debugging ports
- Responds in sub-milliseconds (no network hop)

### Why HTTP and not stdio for MCP transport?

Tauri apps are GUI processes — stdin/stdout aren't available for MCP communication. HTTP/SSE on localhost is the correct transport for a server embedded in an already-running graphical application.

### Why are all tools in one impl block?

The `rmcp` crate's `#[tool_router]` and `#[tool_handler]` macros require all tool methods to be in a single `impl` block. The handler is split across parameter modules for organization, but the dispatch stays monolithic due to this constraint.

### Can multiple agents connect simultaneously?

Yes. The MCP server handles concurrent connections. Each connection gets its own MCP session. State (event log, recorder, bridge) is shared across sessions via `Arc` and thread-safe primitives.

## Browser Extension

### Does the Chrome extension work without the Tauri plugin?

Yes. The Chrome extension (`victauri-browser`) is completely standalone. It provides MCP access to any website — no Tauri app required. It's a separate crate with its own binary.

### Can I use both the plugin and extension together?

Yes. They run on different ports (plugin on 7373, extension on 7474) and serve different purposes:
- Plugin: inspects your Tauri app's webview + backend
- Extension: inspects arbitrary web pages in the browser

### Which browsers are supported?

Chrome, Edge, Brave, and Arc (all Chromium-based browsers that support Manifest V3 and native messaging).

## Troubleshooting

### "Bridge not found" or `__VICTAURI__` is undefined

The JS bridge may not have loaded yet. This can happen if:
- The page is still loading (wait for DOMContentLoaded)
- The webview was created after plugin init (the bridge uses `js_init_script` which only applies to webviews created after the script is registered)
- CSP blocks inline scripts (unlikely with init scripts, but check console errors)

### IPC log is empty

IPC logging works by intercepting `fetch()` calls to `http://ipc.localhost/`. If your IPC log is empty:
- Verify the app has actually made IPC calls (check network tab in dev tools)
- The bridge's fetch interceptor must load before the first IPC call
- `plugin:victauri|*` calls are intentionally excluded from the log

### Recording captures no events

The auto-event recording loop polls `getEventStream()` every 1 second. If your recording appears empty:
- Ensure you called `start` before the actions you want to capture
- Wait at least 1 second after actions before `stop`
- Check that the events you expect (console, mutation, network) are actually occurring

### Tests pass locally but fail in CI

Common CI issues:
- No display server (use `xvfb-run` on Linux for Tauri apps)
- Port conflicts (use a unique port or let the fallback mechanism work)
- Timing (CI machines may be slower — increase timeouts)
- Frontend not built (debug builds embed `frontendDist` at compile time — run `npm run build` first)
