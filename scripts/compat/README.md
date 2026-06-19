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
deps (`.github/actions/linux-deps`), Node, and the JS package managers the targets
use — **pnpm** (En Croissant, Duckling, Lettura) and **Yarn** via Corepack (Kanri).
The `compat.yml` workflow provisions both via Corepack; install them locally before
running the matching app.

In CI, the **Compatibility Retest** workflow (`.github/workflows/compat.yml`) runs
this on demand (`workflow_dispatch`, optionally a single app) and weekly. It is kept
out of the main CI because full frontend + Tauri builds are slow, and because
third-party apps drift on their own schedules — this harness is a best-effort net,
not a release gate.

## Maintaining `apps.json`

Each entry pins `repo`, `ref` (commit SHA), `package_manager`, `frontend_build`
(run from the repo root, must populate `frontendDist`), and `tauri_dir`. Bump `ref`
deliberately so a retest is reproducible against a known app version.

**These apps move, and the moving part is the frontend, not Victauri.** Verified
2026-06-03 (re-pinned 2026-06-17, dropping Surrealist): the targets have drifted
since Victauri's original 2026-05 run, so entries are pinned to **stable release
tags** (not HEAD) and the build recipes are app-specific:

- **Lettura** restructured into a pnpm **monorepo** (`apps/desktop/src-tauri`).
- Package managers differ: **Kanri → yarn** (Nuxt, `.output/public`),
  En Croissant / Duckling / Lettura → **pnpm**.
- The pnpm apps are pnpm-version-sensitive: pnpm ≥10 accepts their settings-only
  `pnpm-workspace.yaml` but **skips native build scripts** (esbuild/swc) unless
  `--config.dangerouslyAllowAllBuilds=true` is passed, while pnpm 9 runs the scripts
  but **rejects** the workspace file. The recipes pin pnpm 10 + allow-all-builds.

When an entry fails at the `frontend` stage, it is almost always the app's own
toolchain (a new bundler, a codegen step, a package-manager bump) — exactly why this
harness exists: so the recipe can be fixed in one place and the result stays
reproducible. The `clone`, `inject` (plugin + capability), `build`-artifact
detection, and `smoke` stages are Victauri's responsibility and are exercised by the
demo-app path (`smoke.sh` validated 15/15 against it).

## Interpreting results

The smoke battery proves Victauri can **attach to and introspect** the app
(webview + DOM + native + tools) — the compatibility question. It is intentionally
narrower than the per-app deep suites described in the docs; deep interaction tests
need app-specific selectors and are not app-agnostic. A `✅ N/N` means Victauri
integrated and every cross-cutting tool worked against that real app, unchanged.
