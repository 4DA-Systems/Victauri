# Getting Started

Get Victauri running in your Tauri app in under 5 minutes.

## Prerequisites

- A Tauri 2.0+ application
- Rust toolchain (stable). Victauri's own MSRV is 1.88 (checked in CI against the committed
  lockfile). Your app's dependency tree can raise that: a fresh project resolving tauri 2.12 needs
  Rust 1.90.
- An MCP client (Claude Code, VS Code, or any MCP-compatible tool)

## Step 1: Add the Dependency

Add `victauri-plugin` to your app's `src-tauri/Cargo.toml`:

```toml
[dependencies]
victauri-plugin = "0.10"
```

The plugin runs inside your app process. In release builds, `init()` returns a no-op plugin thanks to the `#[cfg(debug_assertions)]` gate: no server starts, nothing listens, and `#[inspectable]` emits none of its metadata. Do **not** move it to `[dev-dependencies]` — dev-dependencies are only visible to tests/examples/benches, so the app binary would no longer compile.

What an ordinary dependency still costs a release build is **compile time and some binary
size**, not runtime: the crate and its dependencies (axum, rmcp, ...) are compiled, and the
linker discards the server code it never reaches (Tauri still embeds the plugin's permission
names in the app's ACL) — but the default `bundled-sqlite` feature
compiles SQLite's C code into the build, and it also switches your app's own `rusqlite` (if
any) to bundled SQLite, because Cargo unifies features. Measured on a fresh create-tauri-app
style project: about +1.7 MB on the release executable without LTO, about +58 KB with LTO.
If you want none of that, use the feature-gated setup below.

### Zero release footprint (optional dependency)

`victauri init --optional` scaffolds this for you. It makes Victauri an **optional**
dependency behind a `victauri` Cargo feature, so a build without the feature (your release
build) compiles **no** Victauri code at all:

```toml
[dependencies]
victauri-plugin = { version = "0.10", optional = true }
victauri-core = { version = "0.10", optional = true }   # only if you use #[inspectable]

[features]
victauri = ["dep:victauri-plugin", "dep:victauri-core"]
```

```rust
fn main() {
    with_victauri(tauri::Builder::default())
        // ... your other plugins and setup
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(feature = "victauri")]
fn with_victauri<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder.plugin(victauri_plugin::init()).plugin(
        // A capability FILE naming `victauri:default` would break every build without the
        // feature (Tauri rejects permissions of plugins that are not in the build), so grant
        // it at runtime instead (Tauri's default `dynamic-acl` feature).
        tauri::plugin::Builder::<R>::new("victauri-capability")
            .setup(|app, _api| {
                tauri::Manager::add_capability(
                    app,
                    tauri::ipc::CapabilityBuilder::new("victauri")
                        .window("*")
                        .permission("victauri:default"),
                )?;
                Ok(())
            })
            .build(),
    )
}

#[cfg(not(feature = "victauri"))]
fn with_victauri<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
}
```

Then run the app **with** the feature, and build releases without it:

```bash
pnpm tauri dev --features victauri          # npm: npm run tauri dev -- --features victauri
pnpm tauri build                            # release: feature off, no Victauri code
```

The trade-off is that one flag: a plain `tauri dev` builds without Victauri, and the agent's
tool calls then report the backend unreachable. That is why it is opt-in rather than the
default. (Tauri's `build.features` in `tauri.conf.json` applies to `tauri build` too, so it
cannot turn the feature on for dev only; a `"dev"` script in `package.json` that passes
`--features victauri` is the usual way to make it automatic.) If you annotate commands with
`#[inspectable]`, write it as `#[cfg_attr(feature = "victauri", victauri_plugin::inspectable)]`
so feature-off builds do not need the crate. In this setup, skip Step 3's capability file —
the wiring above grants the permission.

## Step 2: Initialize the Plugin

Add `victauri_plugin::init()` to your Tauri builder in `src-tauri/src/lib.rs` (create-tauri-app's `run()`; older templates use `src-tauri/src/main.rs`):

```rust
fn main() {
    tauri::Builder::default()
        .plugin(victauri_plugin::init())
        // ... your other plugins and setup
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

That's it. In debug builds, this starts an MCP server on `127.0.0.1:7373`. In release builds, it's a no-op.

## Step 3: Add Capabilities

Add the `victauri:default` capability to your app's capabilities file. Create or edit `src-tauri/capabilities/default.json`:

```json
{
  "identifier": "default",
  "windows": ["*"],
  "permissions": [
    "core:default",
    "victauri:default"
  ]
}
```

Without this capability, the Tauri permission system silently blocks IPC callbacks and the plugin cannot interact with your webviews.

## Step 4: Connect via MCP

Create a `.mcp.json` file in your project root (`victauri init` writes this for you):

```json
{
  "mcpServers": {
    "victauri": {
      "command": "victauri",
      "args": ["bridge"]
    }
  }
}
```

This connects through the **`victauri bridge`** — a stdio proxy that discovers the running
app's port at connect time, re-discovers it across restarts, and reads the auth token
automatically from the discovery directory. Because the port is resolved dynamically, the
agent always reaches the right app — even after a rebuild, or when several Victauri apps are
running. (Requires the CLI on your `PATH`: `cargo install victauri-cli`.)

### Multiple apps running at once

Pin the bridge to a specific app by its Tauri bundle identifier — `victauri init` bakes this
in automatically when it can read your `tauri.conf.json`:

```json
{
  "mcpServers": {
    "victauri": {
      "command": "victauri",
      "args": ["bridge", "--app", "com.your.app"]
    }
  }
}
```

The CLI (`victauri check`, `doctor`, `test`, …) run inside a Tauri project, and `cargo test`
run inside the app crate, pick that project's app the same way when several Victauri apps are
running (by the `identifier` in its `tauri.conf.json`). Anywhere else, select one with
`--app <bundle-identifier>` or the `VICTAURI_APP` environment variable.

### Authentication

Auth is **enabled by default** (the token is auto-generated and auto-discovered — the bridge
reads it for you, so no manual header is needed). To pin a fixed token for CI, set it on the
builder with `.auth_token("…")` (or the `VICTAURI_AUTH_TOKEN` env var).

> Connecting with a raw `"url": "http://127.0.0.1:7373/mcp"` also works, but it hardcodes a
> port — if that port is taken (another app, a leftover instance) your agent can silently bind
> the *wrong* app. Prefer the bridge.

## Step 5: Verify It Works

With your app running, check the health endpoint:

Auth is on by default, so every endpoint except `/health` needs the Bearer token. The plugin
writes it (and the bound port) to a per-process, per-user discovery directory
`<root>/<pid>/token` (and `.../port`), where `<pid>` is the app's process id and `<root>` is
`%TEMP%\victauri` on Windows; on macOS/Linux it is `$XDG_RUNTIME_DIR/victauri` when that
directory is private to you, else `<temp>/victauri-<uid>` (`<temp>` = `$TMPDIR` or `/tmp`), or
`~/.local/state/victauri` (`$XDG_STATE_HOME/victauri`) if that one is unusable.

```bash
curl http://127.0.0.1:7373/health
# Returns: {"status":"ok"}  (unauthenticated liveness probe)

# single running app; checks the discovery roots described above, in the plugin's order
if [ "$OS" = Windows_NT ]; then ROOTS="${TMPDIR:-/tmp}/victauri"   # Git Bash: /tmp is %TEMP%
else ROOTS="${XDG_RUNTIME_DIR:+$XDG_RUNTIME_DIR/victauri} ${TMPDIR:-/tmp}/victauri-$(id -u) $HOME/.local/state/victauri"; fi
TOKEN=$(cat "$(for r in $ROOTS; do ls -t "$r"/*/token 2>/dev/null; done | head -n1)")
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:7373/info
# Returns: {"name":"victauri","port":7373,"protocol":"mcp","version":"0.10.2",...}
```

Or use the Victauri CLI:

```bash
cargo install victauri-cli
victauri check
```

## Optional: Register Commands

To enable command discovery and ghost command detection, annotate your Tauri commands with `#[inspectable]` and register them.

The code `#[inspectable]` generates refers to `victauri_core::...` by path, so your app crate
must also depend on `victauri-core` directly (`victauri init` adds it for you) — otherwise
every annotated command fails to compile with `E0433: failed to resolve: use of undeclared
crate victauri_core`:

```toml
[dependencies]
victauri-plugin = "0.10"
victauri-core = "0.10"
```

```rust
use victauri_plugin::inspectable;

#[inspectable(description = "Greet a user", intent = "greeting")]
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

fn main() {
    tauri::Builder::default()
        .plugin(victauri_plugin::init())
        .invoke_handler(tauri::generate_handler![greet])
        .setup(|app| {
            victauri_plugin::register_commands!(app, greet__schema());
            Ok(())
        })
        .run(tauri::generate_context!())
        .unwrap();
}
```

## Optional: REST API

All 35 tools are also available via a REST API with no MCP handshake at all:

```bash
# List available tools
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:7373/api/tools

# Execute a tool directly
curl -X POST http://127.0.0.1:7373/api/tools/eval_js \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"code": "document.title"}'
```

## Next Steps

- [Architecture](./architecture.md) — Understand how Victauri works under the hood
- [Tools Reference](./tools-reference.md) — Complete list of all 35 tools
- [Configuration](./configuration.md) — Customize port, auth, privacy, and more
- [Testing](./testing.md) — Write automated tests with the victauri-test crate
