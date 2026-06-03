# Compatibility retest harness

Re-verifies Victauri against real-world third-party Tauri apps using the **current**
code in this repo — not a published version. The README/docs headline of "96.9%
across 5 apps" was measured on an older Victauri; this harness is what keeps that
claim honest and reproducible.

## What it does (per app)

1. Clones the app at a **pinned commit** (`apps.json`).
2. Injects the current `victauri-plugin` as a path dependency, adds
   `.plugin(victauri_plugin::init())` to the Tauri builder, and writes a
   `capabilities/victauri.json` granting `victauri:default` to all windows.
3. Builds the frontend, then a **debug** Tauri binary (debug → the plugin is active
   and embeds the freshly built `frontendDist`).
4. Launches it headless under `xvfb` and waits for the embedded server + an
   eval-able webview.
5. Runs the app-agnostic smoke battery (`smoke.sh`): webview eval, DOM snapshot with
   refs, element finding, native memory, window list, diagnostics, a11y/perf audits,
   console logs, and a storage round-trip — none of which depend on app-specific
   commands.

It deliberately does **not** modify each app's source beyond the three additive
hooks above, mirroring a real integration (one line in `Cargo.toml`, one line in the
builder, one capability file).

## Run it

```bash
# One app:
scripts/compat/retest-app.sh kanri

# Everything, with a Markdown summary table:
scripts/compat/retest-all.sh

# Keep the clone for debugging:
scripts/compat/retest-app.sh duckling --keep
```

Requires: `git`, `jq`, `curl`, `xvfb`, a Rust toolchain with the Tauri Linux system
deps (`.github/actions/linux-deps`), Node, and `pnpm`.

In CI, the **Compatibility Retest** workflow (`.github/workflows/compat.yml`) runs
this on demand (`workflow_dispatch`, optionally a single app) and weekly. It is kept
out of the main CI because full frontend + Tauri builds for five apps are slow.

## Maintaining `apps.json`

Each entry pins `repo`, `ref` (commit SHA), `frontend_build` (run from the repo
root, must populate `frontendDist`), and `tauri_dir`. The build commands are
best-effort starting points — the first CI run is expected to confirm or adjust them
per app (some apps have monorepo or codegen quirks). Bump `ref` deliberately so a
retest is reproducible against a known app version.

## Interpreting results

The smoke battery proves Victauri can **attach to and introspect** the app
(webview + DOM + native + tools) — the compatibility question. It is intentionally
narrower than the per-app deep suites described in the docs; deep interaction tests
need app-specific selectors and are not app-agnostic. A `✅ N/N` means Victauri
integrated and every cross-cutting tool worked against that real app, unchanged.
