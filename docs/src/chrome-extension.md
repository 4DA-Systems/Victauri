# Chrome Extension

The `victauri-browser` crate provides MCP access to **any website** running in Chrome, Edge, Brave, or Arc — not just Tauri applications.

## What It Does

The Chrome extension + native messaging host extends Victauri's inspection capabilities to regular web pages. An AI agent can connect via MCP on `localhost:7474` and get DOM snapshots, interact with elements, evaluate JavaScript, inspect styles, and more — all on arbitrary websites.

This is useful for:
- Web scraping with semantic understanding
- Cross-site testing workflows
- Automating web tasks that span both Tauri apps and web services
- General browser automation via MCP

## Installation

### 1. Install the Native Host Binary

```bash
cargo install victauri-browser
```

Or build from source:

```bash
cargo build -p victauri-browser --release
```

### 2. Load the Chrome Extension (do this first, to get its ID)

1. Open your browser's extension management page (`chrome://extensions`)
2. Enable "Developer mode"
3. Click "Load unpacked" and select the `extensions/chrome/` directory from the Victauri repo
4. Copy the **extension ID** shown on the extension's card — you need it for the next step

### 3. Register the Native Messaging Host

Register the native-messaging host manifest **with the extension's ID** so the
browser will allow the extension to launch the host:

```bash
victauri-browser-host install <your-extension-id>
```

This writes the native messaging host manifest into your browser's config dir
(Chrome, Edge, Brave, or Arc are auto-detected; on Windows it also writes a
registry key). The manifest's `allowed_origins` is scoped to that extension ID,
so the ID must match the unpacked extension you loaded in step 2. To uninstall:

```bash
victauri-browser-host uninstall
```

### 4. Connect via MCP

The native host starts an HTTP server on `localhost:7474` (with fallback to
7475-7484 if the port is busy). **Auth is on by default**, so a bare `url` will be
rejected with `401` — you must supply the Bearer token. The host either
auto-generates a token (written to the discovery dir,
`<temp>/victauri/<pid>/token`) or you can set a fixed one via the
`VICTAURI_BROWSER_AUTH_TOKEN` environment variable before starting it:

```bash
# Fixed token (recommended for a stable .mcp.json):
VICTAURI_BROWSER_AUTH_TOKEN=my-token victauri-browser-host serve
```

Then point your MCP client at the host port with that token:

```json
{
  "mcpServers": {
    "victauri-browser": {
      "url": "http://127.0.0.1:7474/mcp",
      "headers": { "Authorization": "Bearer my-token" }
    }
  }
}
```

If you let the host auto-generate the token instead, read it from
`<temp>/victauri/<pid>/token` and use it as the Bearer value (the same discovery
file Victauri's own clients read automatically).

## Architecture

The communication flow:

```
MCP Client (Claude Code)
    │
    │ HTTP (localhost:7474)
    ▼
Native Host Binary (victauri-browser)
    │
    │ Chrome Native Messaging (stdio)
    │ 32-bit LE length prefix + UTF-8 JSON
    ▼
Extension Service Worker (MV3)
    │
    │ chrome.tabs.sendMessage()
    ▼
Content Script (ISOLATED world)
    │
    │ CustomEvent (__victauri_command / __victauri_response)
    ▼
JS Bridge (MAIN world)
    │
    │ Direct DOM access
    ▼
Web Page
```

### Components

**Native Host Binary** (`victauri-browser`)
- Dual role: HTTP server for MCP clients AND native messaging host for Chrome
- axum router serves `/mcp` (MCP protocol), `/api/tools` (REST), `/health`, `/info`
- Reads/writes Chrome native messaging format on stdio
- `BridgeDispatch` sends UUID-tagged commands and resolves responses via oneshot channels

**Service Worker** (MV3 background script)
- Manages native messaging connection lifecycle
- Routes commands to the correct tab's content script
- Handles tab lifecycle (creation, removal, navigation)
- Captures screenshots via `captureVisibleTab` (no `debugger`/CDP permission)

**Content Script** (ISOLATED world)
- Relay between service worker and MAIN world bridge
- Uses `CustomEvent` pattern to cross the world boundary
- Injected into all pages matching the extension's permissions

**JS Bridge** (MAIN world, 1700+ lines)
- Full DOM inspection, interactions, accessibility, performance
- Same Playwright-grade actionability checks as the Tauri plugin bridge
- CSS inspection, recording, element finding, scroll, hover, click

## Available Tools (20)

| Tool | Description |
|------|-------------|
| `get_plugin_info` | Extension version and status (handled locally) |
| `tabs.list` | List open browser tabs (handled locally) |
| `dom_snapshot` | Full accessible DOM tree of active tab |
| `find_elements` | Search by CSS selector, text, or role |
| `eval_js` | Evaluate JavaScript in page context |
| `click` | Click an element by ref |
| `fill` | Set input value |
| `type_text` | Type characters one-by-one |
| `press_key` | Dispatch keyboard event |
| `hover` | Hover over element |
| `scroll_into_view` | Scroll element into viewport |
| `get_styles` | Computed CSS for an element |
| `get_bounding_boxes` | Element dimensions and box model |
| `highlight_element` | Draw debug overlay |
| `clear_highlights` | Remove all overlays |
| `screenshot` | Capture visible tab as PNG |
| `navigate` | Go to URL |
| `get_cookies` | Get cookies for current domain |
| `get_console_logs` | Captured console entries |
| `get_network_log` | Fetch/XHR request history |

## Authentication

The native host supports Bearer token authentication:

```bash
# Set via environment variable (note: the browser host uses its own
# VICTAURI_BROWSER_AUTH_TOKEN var, not the plugin's VICTAURI_AUTH_TOKEN)
VICTAURI_BROWSER_AUTH_TOKEN=my-token victauri-browser-host serve
```

When no token is set, the host auto-generates one and writes it to the
discovery directory (`<temp>/victauri/<pid>/token`, user-only permissions) so a
client can read it instead of scraping the process log.

Security features:
- Constant-time token comparison
- Token-bucket rate limiter
- Security headers on all responses
- Origin guard (blocks non-localhost origins)

## Trust Model (read this)

Browser mode is **experimental, cooperative automation.** Three things to understand:

**Channel integrity (audit A4 — fixed in 0.7.9).** The content script relays commands and
responses through the page's own JavaScript context (`window` `CustomEvent`s in the MAIN
world), which a page can observe. As of 0.7.9 both directions are authenticated with an
HMAC-SHA256 keyed by a nonce exchanged during `document_start` before any page script runs,
so a hostile page can **no longer** inject a command or race a forged response — the relay
and bridge reject anything without a valid MAC. Verified in a real browser
(`extensions/chrome/tests/e2e/a4-channel-forgery.mjs`).

**Secure context required.** Web Crypto (`crypto.subtle`), which the authenticated channel
needs, only exists in a secure context. The bridge works on **https://** pages and on
**http://localhost / 127.0.0.1**, but **fails closed on a plain `http://` origin** — it
refuses to operate rather than fall back to an unauthenticated channel. If the extension
goes silent on an http site, that is why; use https or the Tauri plugin.

**Still not a full security boundary.** There is not yet a per-domain/tab privilege model
or output redaction for browser mode, and the bridge shares the page's MAIN world. So while
the channel is now tamper-resistant, treat browser mode as automation for pages you are
inspecting deliberately. For results that must be robust regardless of the page, use the
**Tauri plugin** path: it runs in the Rust process below the webview and reads backend /
IPC / DB state directly (`verify_state` / `query_db` / IPC-history are the verification
surface).

## Port Behavior

Default port: `7474`. If busy, tries `7475` through `7484`. The `victauri-browser-host serve` command prints the actual port on startup.

## Tab Management

The extension tracks tab state (URL, title, bridge readiness). Commands are sent to the **active tab** by default, or you can target a specific tab by ID using the `tab_id` parameter where supported.

Special behaviors:
- Navigation uses `chrome.tabs.update()` (not content script `window.location`) for reliability
- Cookies use `chrome.cookies.getAll()` for httpOnly access
- Screenshots use Chrome's `chrome.tabs.captureVisibleTab()` API
