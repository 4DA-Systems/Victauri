# victauri-cli

CLI toolkit for [Victauri](https://github.com/4DA-Systems/victauri) — scaffold tests, diagnose setup, check running apps, record sessions, measure coverage.

## Install

```bash
cargo install victauri-cli
```

## Commands

### `victauri init [--path <dir>] [--client <clients>] [--optional]`

Zero-config setup for any Tauri project. Detects your project, adds dependencies, auto-patches your Tauri builder to wire the plugin, creates capabilities, generates starter test files, and wires your AI agent client.

```bash
victauri init
victauri init --path ./my-tauri-app
victauri init --client claude,cursor,vscode
victauri init --optional        # zero release footprint: Victauri behind a `victauri` feature
```

What it does:
1. Adds `victauri-plugin` and `victauri-core` to `Cargo.toml`
2. Patches `src/main.rs` or `src/lib.rs` to insert `.plugin(victauri_plugin::init())`
3. Creates `capabilities/victauri.json` for Tauri permissions
4. Generates `tests/smoke.rs` and `tests/integration.rs` templates and a CI workflow
5. Wires the agent client(s) exactly like `victauri setup` (default: Claude Code — `.mcp.json`, a CLAUDE.md block and a `.claude/skills/victauri/SKILL.md` skill)

### `victauri setup <clients…>`

Wire Victauri into AI agent clients only — no Cargo or test scaffolding. Each client gets
`victauri bridge --app <identifier>` (identifier read from `tauri.conf.json`) plus agent
instructions.

```bash
victauri setup cursor vscode        # or: victauri setup all
victauri setup windsurf --user-config
victauri setup all --dry-run        # show what would change
```

| Client | MCP config written | Instructions |
|---|---|---|
| `claude` | `.mcp.json` (`mcpServers`) | CLAUDE.md block + `.claude/skills/victauri/SKILL.md` |
| `cursor` | `.cursor/mcp.json` (`mcpServers`, `type: stdio`) | AGENTS.md block |
| `vscode` (alias `copilot`) | `.vscode/mcp.json` (`servers`, `type: stdio`) | AGENTS.md block |
| `windsurf` (alias `devin`) | user-level `mcp_config.json` — needs `--user-config` | AGENTS.md block |
| `codex` | `.codex/config.toml` (`[mcp_servers.victauri]`; Codex loads it for trusted projects), or `~/.codex/config.toml` with `--user-config` | AGENTS.md block |
| `gemini` | `.gemini/settings.json` (`mcpServers`) | AGENTS.md block + a GEMINI.md block that imports it |
| `zed` | `.zed/settings.json` (`context_servers`) | AGENTS.md block |
| `roo` | `.roo/mcp.json` (`mcpServers`) | AGENTS.md block |

Existing files are merged, never clobbered: the entry is inserted as text, so comments
(JSONC), formatting and key order survive, and the result is re-parsed and verified before it
is written. An existing `victauri` entry is left exactly as it is, so re-running changes
nothing. A malformed file is left untouched and the entry to add by hand is printed. Nothing
outside the project is written without `--user-config`. Opt out by deleting the entry, the
`VICTAURI:BEGIN … VICTAURI:END` block, or `.claude/skills/victauri/`.

With `--optional`, the dependencies are `optional = true` behind a `victauri` Cargo feature, the
builder is wrapped as `with_victauri(tauri::Builder::default())` (a function generated for both
feature states), and the `victauri:default` permission is granted at runtime instead of by a
capability file — a file naming it would break every build without the feature. That runtime
grant (`tauri::Manager::add_capability`) needs Tauri's `dynamic-acl` feature, which is on by
default; if your app depends on `tauri` with `default-features = false`, add `dynamic-acl` to its
features. Builds without the feature, such as `tauri build`, then contain no Victauri code. Start
the app with `npm run tauri dev -- --features victauri` (or `pnpm tauri dev --features victauri`).
The generated CLAUDE.md / AGENTS.md / skill say so too, and `victauri setup` adds the same note
when it finds `victauri-plugin` is an optional dependency. If your app already gates
`victauri-plugin` behind a feature of its own (e.g. `devtools = ["dep:victauri-plugin"]`),
`init` and `setup` use that feature's name instead of adding `victauri`.

### `victauri doctor`

Comprehensive diagnostic — checks every step from project structure to live tool operation. Run this when something isn't working.

```bash
victauri doctor
```

Checks: project structure, Tauri dependency, plugin dependency, test dependency, plugin wiring in source, an agent client configured (any project-level MCP config with a `victauri` server that runs `victauri bridge` — one that runs anything else is a warning), capabilities, test files, server connectivity, plugin info, JS bridge, DOM snapshot, IPC integrity.

### `victauri check`

Connect to a running Tauri app and report health — IPC integrity, ghost commands, memory usage.

```bash
victauri check
victauri check --junit report.xml   # JUnit XML output for CI
victauri check --app com.example.app  # pick one of several running apps
```

**Several Victauri apps running?** `check`, `test`, `invoke`, `call`, `tools`, `coverage`, `doctor` and `record`
refuse to guess: they list each running app as `identifier (port N, pid P)` and ask you to
select one with `--app <bundle-identifier>` (exact identifier or product name — never a
prefix) or the `VICTAURI_APP` env var, or to pin the port with `VICTAURI_PORT`. A
`VICTAURI_AUTH_TOKEN` without `VICTAURI_PORT` is only sent to the running app whose own
discovery token matches it.

### `victauri test`

Run the built-in smoke test suite (11 checks) against a running app. Exits 0/1 for CI.

```bash
victauri test
victauri test --max-load-ms 3000    # Custom load time budget
victauri test --max-heap-mb 200     # Custom heap budget
victauri test --junit results.xml   # JUnit XML output
```

### `victauri record`

Record user interactions from a running app and generate a Rust test file.

```bash
victauri record                             # Direct client API style
victauri record --locator                   # Locator API style output
victauri record --assert-ipc               # Add IPC assertion calls
victauri record --output tests/login.rs     # Write to specific file
victauri record --test-name login_flow      # Custom test function name
```

With `--locator`, generated code uses `Locator::test_id("btn").click(&mut client)` instead of `client.click_by_id("btn")`. With `--assert-ipc`, each IPC command seen during recording gets an `assert_ipc_called()` assertion at the end.

### `victauri tools` / `victauri call`

Every Victauri tool from a plain shell — for agents and scripts that work through a terminal
rather than an MCP client (Codex without MCP, Pi, OpenClaw, CI). Same discovery, trust checks,
app pinning and auth as every other command; no port, URL or token to handle.

```bash
victauri tools                          # every tool, one line each (live from the running app)
victauri tools query_db                 # a tool's description and arguments
victauri tools --json                   # MCP tool objects with their input schemas

victauri call dom_snapshot
victauri call query_db query="SELECT count(*) FROM items" max_rows:=10
victauri call invoke_command command=get_settings
victauri call interact action=click ref_id=e12
victauri call screenshot --out shot.png # images are saved, never printed as base64
echo '{"action":"list"}' | victauri call window --args-file -
```

Arguments: `key=value` is a string, `key:=json` any JSON value, a leading `'{...}'` a JSON
object, `--args-file <path>` (`-` = stdin) a JSON object from a file; later sources override
earlier ones. In Windows PowerShell, which strips the quotes inside JSON arguments to native
programs, use pairs or `--args-file -`.

Output is the tool's result as JSON on stdout (`--raw` for one line). Exit codes: `0` success,
`1` the tool reported an error (`{"error": …}` on stdout, the message on stderr), `2` bad
arguments or no app reachable. With no app running, `victauri tools` still lists the built-in
tool names and descriptions.

### `victauri coverage`

Report IPC command coverage — which registered commands your tests exercise.

```bash
victauri coverage                    # Print coverage report
victauri coverage --threshold 80     # Exit code 1 if below 80%
victauri coverage --junit cov.xml    # JUnit XML output
```

### `victauri watch`

Watch test files and re-run on changes — with 300ms debounce.

```bash
victauri watch                           # Watch default test directory
victauri watch --dir tests/integration   # Watch specific directory
victauri watch --filter greet            # Only run matching tests
```

## Documentation

Full API docs: [docs.rs/victauri-cli](https://docs.rs/victauri-cli)

MCP Registry name: `mcp-name: io.github.4DA-Systems/victauri`

## License

Apache-2.0 -- see [LICENSE](../../LICENSE)

Part of [Victauri](https://github.com/4DA-Systems/victauri). Built by [4DA Systems](https://4da.ai).
