# Backend Logs

Victauri's other log tools see the **webview** (JS console, fetch, IPC). This page is about
the **Rust side** — the debug console a developer watches while the app runs — and how an
agent reads it, filters it, waits on it, and follows it live.

## What you get

| Tool call | What it returns |
|---|---|
| `logs {action:"backend_digest"}` | **Start here.** Per-level counts, noisiest targets, repeated messages collapsed into templates (`Registered source` ×22), the most recent warnings/errors, and every captured panic. One cheap call instead of paging thousands of lines. |
| `logs {action:"backend", level, target, filter, since_seq, limit}` | Structured entries: level, target, message, **typed fields** (`elapsed_ms: 41191`), **span context** (`pipeline_run{batch=50}`), thread, `file:line`. Page forward with `next_seq` → `since_seq`. |
| `wait_for {condition:"log", value, level, fields, since_seq}` | Blocks until a matching line is logged — woken the moment the line is captured (edge-triggered, no polling), with a `since_ms` look-back (default 2 s) so a line logged just before the call still counts. A timeout reports `missed_entries` if lines were evicted before the wait could check them. |
| `invoke_command {command, args, with_logs:true}` | The command's result **plus** every backend line (debug and above) logged while it ran — what the Rust side said about the call, in the same round trip. A failing call (an error from the command, a timeout) carries the lines too, in its error result. Needs the `logs.backend` capability. |
| `logs {action:"stdout"}` | Raw stdout/stderr lines, when the app was launched with `victauri run` (below) — including the process's exit record. |

From a terminal (and from any agent harness that turns command output lines into events):

```sh
victauri logs --digest                 # the digest, formatted
victauri logs --level warn -n 50       # recent warnings/errors
victauri logs --follow --level warn    # one line per new warning/error, as they happen
victauri logs --stdout                 # the `victauri run` capture — works after a crash
```

`--follow` survives a busy app and a restart: when a read fails it re-resolves the app through
discovery for up to 20 s (a `tauri dev` rebuild comes back with a new token and port) and then
follows the new process — first printing how the previous one ended (its exit record and last
stderr lines, and any warning or error) when that process had its own `victauri run` capture.
If the app is gone for good, it switches to that app's `victauri run` capture to print its last
words and exit status; an app not launched with `victauri run` has no capture, and the follower
says so rather than showing another app's.

## Capture sources

There are four; use whichever fits the app. They can be combined.

| Source | App change | Sees | Survives a crash |
|---|---|---|---|
| Panic hook | **none** (installed by the plugin in debug builds; `VictauriBuilder::capture_panics(false)` to opt out) | every Rust panic, incl. background threads and Tokio tasks | the entry is in memory — read it before the process exits |
| `victauri run -- <cmd>` | **none** | everything printed to stdout/stderr: `println!`, any logger's console output, C libraries, `cargo` build errors of `tauri dev` | **yes** — out of process |
| `victauri_plugin::log_layer()` | one `.with(...)` in the `tracing` setup | every `tracing` event (and `log` records bridged by `tracing-log`), structured | no |
| `victauri_plugin::log_logger()` / `wrap_logger()` | one line in the `log` setup | every `log` record, structured | no |

Why both in-process and out-of-process? An in-process capture dies with the process: what the
app writes right before an `abort()` or a hard crash never reaches an in-process buffer that
somebody can still read, while `victauri run`'s parent process owns the pipe and keeps it. So
crash evidence comes from `victauri run`; structure (fields, spans, cheap filtering) comes from
the in-process layer.

### Recipes by logging stack

Tauri apps typically log through `tauri-plugin-log`, a `tracing-subscriber` registry, another
`log` backend, or plain `println!` — and most also print to stdout, which is why `victauri run`
covers almost everything.

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

All of these are no-ops in release builds (`log_layer()` returns `None` and its layer type is
empty, so the line adds no Victauri code to a release binary; `wrap_logger` returns the inner
logger; `log_logger()` discards), and when `VICTAURI_DISABLE=1` is set.

**Victauri behind a Cargo feature** (`victauri init --optional`): the added line names
`victauri_plugin`, so gate it like the plugin itself:

```rust
let registry = tracing_subscriber::registry()
    .with(tracing_subscriber::EnvFilter::new("info"))
    .with(tracing_subscriber::fmt::layer());
#[cfg(feature = "victauri")]
let registry = registry.with(victauri_plugin::log_layer());
registry.init();
```

## Limits (honest)

- **What the app's filter lets through.** A layer added with `.with(...)` sees what passes the
  subscriber's *global* filter (an `EnvFilter` added with `.with(...)` filters every layer). If
  the app runs at `info`, debug lines are not captured. On top of that the capture keeps
  `debug` and above by default (`VICTAURI_LOG_LEVEL=trace|debug|info|warn|error` to change it);
  the layer never filters anything for the app's own layers. A filter read from the
  environment (`EnvFilter::from_default_env()`, `env_logger::Builder::from_default_env()`) is
  error-only while `RUST_LOG` is unset, so such an app captures nothing below `error` until
  `RUST_LOG` is set; the tools then report the source as installed
  (`sources.structured_installed`) and say so, instead of asking for a line the app already has.
- **One process.** The plugin sees its own process. Sidecars and helper binaries (e.g. a
  separate engine process) need their own capture — `victauri run` them too.
- **In-memory and bounded.** The buffer keeps 5,000 entries / ~8 MB by default
  (`VICTAURI_LOG_CAPACITY`, up to 100,000 entries, to change it); `evicted` and a `gap` flag say
  when a cursor fell behind (a `logs clear` is not a gap). It is not a log archive — keep the
  app's own file logging for that.
- **Victauri's own lines are excluded** (`victauri_*`, the embedded `rmcp` SDK): every agent
  request produces some, so capturing them would let reading the log generate log noise. Set
  `VICTAURI_LOG_INTERNAL=1` to keep them. Tokio's own runtime instrumentation (`tokio::*`,
  `runtime::*` — what `tokio_unstable` emits for tokio-console) is always excluded: part of it is
  emitted while Tokio holds internal locks.
- **Panics.** The hook is installed when the plugin builds (debug builds) and chains the hook
  installed before it, which runs **first** — an app's own crash hook (zeroising secrets,
  writing a crash log) is never delayed by Victauri's. An app that installs a hook *later*
  without chaining replaces Victauri's; `sources.panic_hook` then still says it was installed.
  The first 20 panics carry a backtrace: its frames are captured on the panicking thread, but
  symbolised (the slow part) only when an agent reads the entry. On Windows that needs the
  binary's `.pdb` beside it (as for any Rust backtrace): a copied `.exe` shows `<unknown>` frames.
- **Secrets.** Backend logs can contain whatever the app logs. Output passes through the same
  redaction as every other tool — on in the `Observe` and `Test` privacy profiles and with
  `strict_privacy_mode()`, off in the default `FullControl` profile — including `key=value`
  pairs in span context and messages, and `filter` / `fields` / `wait_for` match the redacted
  text (a match can never confirm a hidden value). The actions are gated like every other
  (`logs.backend` / `logs.backend_digest` / `logs.stdout`); `wait_for log` and
  `invoke_command {with_logs}` need the same capability, and disabling the whole `logs` tool
  covers them too. The capture file written
  by `victauri run` is created owner-only in a `console` directory under the private per-user
  Victauri directory, which must be a real directory owned by you and not group/other-writable
  (the same check discovery uses; on Windows it also gets an owner-only ACL) — never a shared
  temp directory. A relative `--capture` path is made absolute (the app may run from another
  directory).
- **Terminal safety.** `victauri logs` prints one entry per line with control and bidi
  characters escaped (an app's ANSI colours are stripped), so log text cannot drive the
  terminal or forge a CI annotation.
- **Cost.** Each captured event is formatted once into the buffer (values are bounded while
  they are formatted, so a multi-megabyte argument costs a few kilobytes). For a very chatty
  app, raise the level with `VICTAURI_LOG_LEVEL=info`.
- **Keep the launcher alive.** `victauri run` owns the app's stdout/stderr pipes. Ctrl+C reaches
  both; but if the launcher alone is killed, the app's next console write fails (on Unix a
  `println!` then panics on the broken pipe).

## A server that stays up when the app hangs

Victauri's server runs on its **own** Tokio runtime (two dedicated threads), not on Tauri's
shared one (since 0.10.1). When an app pins its async workers — a blocking call inside an
`async` command is the classic bug — backend tools keep answering, `logs backend` included (the
demo app's E2E test `server_stays_responsive_while_the_app_blocks_its_async_runtime` checks
this on every CI run). Tools that need the app's main thread or webview still wait on them.
