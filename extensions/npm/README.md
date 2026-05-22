# victauri-browser

Native messaging host for the Victauri Chrome/Firefox/Edge extension. Provides an MCP (Model Context Protocol) bridge that gives AI agents full access to any website through the browser.

## Installation

```bash
npm install -g victauri-browser
```

Or run directly without installing:

```bash
npx victauri-browser install
```

This will:

1. Download the pre-built `victauri-browser-host` binary for your platform
2. Register the native messaging host manifest with Chrome, Edge, and Brave

## Supported Platforms

| Platform | Architecture |
|----------|-------------|
| Linux    | x86_64, aarch64 |
| macOS    | x86_64 (Intel), aarch64 (Apple Silicon) |
| Windows  | x86_64 |

## Usage

### Register the native messaging host

```bash
victauri-browser install [extension-id]
```

Registers the native messaging host manifest so the Victauri Chrome extension can communicate with the MCP server. Optionally pass a specific extension ID (defaults to the published Victauri extension ID).

### Start the MCP server

```bash
victauri-browser serve
```

Starts the MCP server on `http://127.0.0.1:7474/mcp`. This is normally started automatically by the Chrome extension via native messaging, but can be run manually for debugging.

### Unregister

```bash
victauri-browser uninstall
```

Removes the native messaging host registration from all browsers.

### Version

```bash
victauri-browser version
```

## Configuration

| Environment Variable | Description | Default |
|---------------------|-------------|---------|
| `VICTAURI_BROWSER_PORT` | Port for the MCP HTTP server | `7474` |
| `VICTAURI_BROWSER_AUTH_TOKEN` | Bearer token for API authentication | Auto-generated |

## Connecting an MCP Client

Add to your `.mcp.json`:

```json
{
  "mcpServers": {
    "victauri-browser": {
      "url": "http://127.0.0.1:7474/mcp"
    }
  }
}
```

## How It Works

```
MCP Client (Claude Code, etc.)
    |
    | HTTP (Streamable HTTP + SSE)
    v
victauri-browser-host (:7474)
    |
    | Native Messaging (stdio, Chrome protocol)
    v
Chrome Extension (Service Worker)
    |
    | Content Script relay (CustomEvent)
    v
Web Page (DOM access, JS execution, etc.)
```

The native messaging host serves as a bridge between MCP clients and the Chrome extension. It translates MCP tool calls into native messaging commands that the extension's service worker processes, then relays results back.

## Available Tools

The MCP server exposes 20 tools for browser interaction:

- **dom_snapshot** — Full accessible DOM tree with ref handles
- **find_elements** — Search elements by CSS selector or text
- **interact** — Click, hover, focus, scroll, double-click
- **input** — Fill, type text, press keys
- **eval_js** — Execute JavaScript in the page context
- **screenshot** — Capture page screenshot (via CDP)
- **inspect** — CSS styles, bounding boxes, accessibility audit, performance metrics
- **navigate** — Go to URL, back, forward, history
- **tabs** — List, create, close, activate tabs
- **storage** — localStorage, cookies
- And more...

## Uninstalling

```bash
npm uninstall -g victauri-browser
```

This automatically unregisters the native messaging host before removing the package.

## Building from Source

If you prefer to build from source instead of using pre-built binaries:

```bash
cargo install victauri-browser
victauri-browser-host install
```

## Documentation

- [Victauri GitHub](https://github.com/runyourempire/victauri)
- [Chrome Extension](https://github.com/runyourempire/victauri/tree/main/extensions/chrome)

## License

Apache-2.0
