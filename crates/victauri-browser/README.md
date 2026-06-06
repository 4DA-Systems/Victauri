# victauri-browser

Native messaging host + browser extension that brings [Victauri](https://github.com/runyourempire/victauri)'s MCP inspection to **any website** -- not just Tauri apps.

## How It Works

```
MCP Client → axum HTTP :7474 → Native Messaging (stdio) → Browser Extension → Content Script → Page
```

The Rust binary serves dual roles: HTTP server for MCP clients AND native messaging host for the browser extension. The extension injects a JS bridge into every page, giving agents the same DOM/interaction/inspection tools that `victauri-plugin` provides inside Tauri apps.

## Install

```bash
cargo install victauri-browser
victauri-browser-host install    # Registers native messaging host for Chrome/Edge/Brave/Arc
```

Then load `extensions/chrome/` (or `extensions/firefox/`) as an unpacked extension in your browser.

## Connect

Point any MCP client at `http://127.0.0.1:7474/mcp`, or use the REST API:

```bash
# List available tools
curl http://127.0.0.1:7474/api/tools

# Take a DOM snapshot of the active tab
curl -X POST http://127.0.0.1:7474/api/tools/dom_snapshot
```

## Tools

20 MCP tools available through the extension:

| Tool | What it does |
|------|-------------|
| `dom_snapshot` | Full accessible DOM tree with ref handles |
| `find_elements` | Search by text, role, selector, test ID |
| `click` | Click an element (with actionability checks) |
| `fill` | Set input/textarea value |
| `type_text` | Type character-by-character with key events |
| `press_key` | Dispatch keyboard events (Enter, Tab, Escape, combos) |
| `hover` | Trigger mouse hover events |
| `eval_js` | Execute JavaScript in the page |
| `screenshot` | Capture visible tab as PNG |
| `get_styles` | Computed CSS for any element |
| `get_bounding_boxes` | Pixel measurements with box model |
| `highlight_element` | Visual debug overlay |
| `clear_highlights` | Remove overlays |
| `audit_accessibility` | WCAG compliance check |
| `get_performance` | Navigation timing, heap, resources |
| `wait_for` | Poll for conditions (selector, text, URL) |
| `navigate` | Go to URL, go back |
| `get_cookies` | Read cookies for the current domain |
| `tabs.list` | List open browser tabs |
| `get_plugin_info` | Host version and status |

## Security

- **Localhost only** -- `127.0.0.1`, never `0.0.0.0`
- **Bearer auth** -- on by default; auto-generated token written to the discovery dir (`<temp>/victauri/<pid>/token`), or set a fixed token via the `VICTAURI_BROWSER_AUTH_TOKEN` env var
- **Rate limiting** -- token-bucket rate limiter
- **Origin guard** -- rejects cross-origin requests
- **Port fallback** -- tries :7474-7484 if the preferred port is taken

## CLI

```bash
victauri-browser-host install      # Register native messaging host
victauri-browser-host uninstall    # Remove registration
victauri-browser-host serve        # Start HTTP server without native messaging
victauri-browser-host version      # Print version
```

## Browser Support

| Browser | Extension | Status |
|---------|-----------|--------|
| Chrome / Edge / Brave / Arc | `extensions/chrome/` | Full support (MV3 + CDP screenshots) |
| Firefox | `extensions/firefox/` | Full support (MV3, `browser.*` namespace, no CDP) |

## Tests

```bash
cargo test -p victauri-browser                         # 99 Rust tests
cd extensions/chrome/tests && npx vitest run           # 163 JS bridge tests
```

## Documentation

Full API docs: [docs.rs/victauri-browser](https://docs.rs/victauri-browser)

## License

Apache-2.0 -- see [LICENSE](../../LICENSE)

Part of [Victauri](https://github.com/runyourempire/victauri). Built by [4DA Systems](https://4da.ai).
