# Backend Logs

Victauri's other log tools see the **webview** (JS console, fetch, IPC). This page is about
the **Rust side** — the debug console a developer watches while the app runs — and how an
agent reads it, filters it, waits on it, and follows it live.

## What you get

| Tool call | What it returns |
|---|---|
| `logs {action:"backend_digest"}` | **Start here.** Per-level counts, noisiest targets, repeated messages collapsed into templates (`Registered source` ×22), the most recent warnings/errors, and every captured panic. One cheap call instead of paging thousands of lines. |
| `logs {action:"backend", level, target, filter, since_seq, limit}` | Structured entries: level, target, message, **typed fields** (`elapsed_ms: 41191`), **span context** (`pipeline_run{batch=50}`), thread, `file:line`. Page forward with `next_seq` → `since_seq`. |
| `wait_for {condition:"log", value, level}` | Blocks until a matching line is logged — wakes within a fraction of a millisecond of the event (edge-triggered, no polling), with a `since_ms` look-back (default 2 s) so a line logged just before the call still counts. |
| `invoke_command {command, args, with_logs:true}` | The command's result **plus** every backend line (debug and above) logged while it ran — what the Rust side said about the call, in the same round trip. |
| `logs {action:"stdout"}` | Raw stdout/stderr lines, when the app was launched with `victauri run` (below) — including the process's exit record. |

From a terminal (and from any agent harness that turns command output lines into events):

```sh
victauri logs --digest                 # the digest, formatted
victauri logs --level warn -n 50       # recent warnings/errors
victauri logs --follow --level warn    # one line per new warning/error, as they happen
victauri logs --stdout                 # the `victauri run` capture — works after a crash
```

`--follow` survives a busy app (it re-probes before declaring the app gone) and, if the app
dies, switches to that app's `victauri run` capture to print its last words and exit status.

## Capture sources

There are four; use whichever fits the app. They can be combined.

| Source | App change | Sees | Survives a crash |
|---|---|---|---|
| Panic hook | **none** (installed by the plugin in debug builds) | every Rust panic, incl. background threads and Tokio tasks | the entry is in memory — read it before the process exits |
| `victauri run -- <cmd>` | **none** | everything printed to stdout/stderr: `println!`, any logger's console output, C libraries, `cargo` build errors of `tauri dev` | **yes** — out of process |
| `victauri_plugin::log_layer()` | one `.with(...)` in the `tracing` setup | every `tracing` event (and `log` records bridged by `tracing-log`), structured | no |
| `victauri_plugin::log_logger()` / `wrap_logger()` | one line in the `log` setup | every `log` record, structured | no |

Why both in-process and out-of-process? An in-process capture dies with the process: in
testing, a line written right before `abort()` was lost **20/20 times** when captured inside
the process, and delivered **20/20 times** when the parent process owned the pipe. So crash
evidence comes from `victauri run`; structure (fields, spans, cheap filtering) comes from the
in-process layer.

### Recipes by logging stack

A survey of 29 open-source Tauri 2 apps found `tauri-plugin-log` (13), `tracing-subscriber`
registries (9), a handful of other `log` backends, and a few `println!`-only apps. Nearly all
of them also print to stdout — which is why `victauri run` covers almost everything.

**`tracing` with a registry** (the common case):

```rust
use tracing_subscriber::prelude::*;

tracing_subscriber::registry()
    .with(tracing_subscriber::EnvFilter::new("info"))
    .with(tracing_subscriber::fmt::layer())
    .with(victauri_plugin::log_layer()) // ← added
    .init();
```

**`tracing_subscriber::fmt().init()`** — keep the builder, add the layer before `init`:

```rust
use tracing_subscriber::prelude::*;

tracing_subscriber::fmt()
    .with_env_filter("info")
    .finish()
    .with(victauri_plugin::log_layer()) // ← added
    .init();
```

**`tauri-plugin-log`** — add a dispatch target (`fern` is re-exported by the plugin):

```rust
use tauri_plugin_log::{fern, Target, TargetKind};

tauri_plugin_log::Builder::new()
    .target(Target::new(TargetKind::Dispatch(
        fern::Dispatch::new().chain(victauri_plugin::log_logger()), // ← added
    )))
    .build()
```

**`env_logger` / any `log::Log`** — wrap the logger you already install:

```rust
let inner = env_logger::Builder::from_default_env().build();
let max = inner.filter();
log::set_boxed_logger(victauri_plugin::wrap_logger(Box::new(inner))).ok();
log::set_max_level(max);
```

**`fern`** — `.chain(victauri_plugin::log_logger())`.

**`println!` only / anything else** — launch through the capture launcher:

```sh
victauri run -- npm run tauri dev
victauri run -- cargo tauri dev
victauri run -- ./src-tauri/target/debug/my-app
```

All of these are no-ops in release builds (`log_layer()` returns `None`, `wrap_logger`
returns the inner logger, `log_logger()` discards), and when `VICTAURI_DISABLE=1` is set.

## Limits (honest)

- **What the app's filter lets through.** A layer added with `.with(...)` sees what passes the
  subscriber's *global* filter (an `EnvFilter` added with `.with(...)` filters every layer). If
  the app runs at `info`, debug lines are not captured.
- **One process.** The plugin sees its own process. Sidecars and helper binaries (e.g. a
  separate engine process) need their own capture — `victauri run` them too.
- **In-memory and bounded.** The buffer keeps 5,000 entries / ~8 MB by default
  (`VICTAURI_LOG_CAPACITY` to change it); `evicted` and a `gap` flag say when a cursor fell
  behind. It is not a log archive — keep the app's own file logging for that.
- **Victauri's own lines are excluded** (`victauri_*`, the embedded `rmcp` SDK): every agent
  request produces some, so capturing them would let reading the log generate log noise. Set
  `VICTAURI_LOG_INTERNAL=1` to keep them.
- **Secrets.** Backend logs can contain whatever the app logs. Output passes through the same
  redaction as every other tool (on by default in the `Observe` and `Test` privacy profiles),
  the capture file written by `victauri run` is created owner-only on Unix and lives in the
  per-user temp directory.
- **Cost.** One captured event costs a few microseconds in a debug build (measured ~3.6 µs) —
  far less than the console write the app already pays (0.5–8 ms per line to a Windows
  console). A panic's backtrace is resolved only for the first 20 panics.

## A server that stays up when the app hangs

Victauri's server runs on its **own** Tokio runtime (two dedicated threads), not on Tauri's
shared one. When an app pins its async workers — a blocking call inside an `async` command is
the classic bug — backend tools keep answering: measured on the demo app, `/health` went from
~9 s to ~75 ms under that load, and `logs backend` showed the offending tasks' own warnings
while they blocked. Tools that need the app's main thread or webview still wait on them.
