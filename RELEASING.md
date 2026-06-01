# Releasing Victauri

Each surface ships **independently, on its own tag** — push a tag, a pipeline does
the rest (test → build → publish). No surface can silently rot, because the only way
it updates is a tag that runs the full build+publish, and a weekly **Surface Audit**
fails loudly if any published version falls behind the repo.

## Required GitHub secrets (Settings → Secrets and variables → Actions)

| Secret | Used by | Scope |
|---|---|---|
| `CARGO_REGISTRY_TOKEN` | core release → crates.io | crates.io API token |
| `VSCE_PAT` | VS Code release → Marketplace | Azure DevOps PAT, *Marketplace → Manage* (publisher `4da-systems`) |
| `OVSX_TOKEN` *(optional)* | VS Code release → Open VSX | open-vsx.org token (Cursor / VSCodium / agent IDEs) |
| `NPM_TOKEN` | npm release | npmjs automation token, publish rights on `victauri-browser` |

## Before you tag — the pre-publish gate

0.7.4 shipped with two bugs (a macOS-only discovery bug and a real-app E2E failure) because
the release's own test step runs **only on ubuntu** and publish wasn't gated on the full CI.
Two layers now prevent that:

1. **Local preflight** — run `./scripts/preflight.ps1` (Windows) or `./scripts/preflight.sh`
   before pushing. It runs fmt + clippy + the full workspace tests + the Chrome bridge tests,
   so fast failures are caught locally in seconds instead of a 16-minute CI round trip. It
   **cannot** catch macOS/Linux-only bugs (you're on one OS) or the real-app E2E — that's CI.
2. **`require-ci-green` (the hard gate)** — `release.yml` will **not** publish unless the
   **full CI** (all platforms + real-app E2E) concluded **success** for the exact release
   commit. A tag on a non-CI-green commit is refused, loudly, before the publish step.

**Recommended flow:** `preflight` → push to `main` → **wait for CI green on all platforms** →
bump + tag. (Pushing commit + tag together still works — the gate waits for CI to finish, up
to ~40 min, then requires success.)

## Core (crates + binaries) — `v*`

```bash
./scripts/bump-version.sh 0.8.0     # updates the workspace version everywhere
# edit CHANGELOG.md, commit
git tag v0.8.0 && git push origin main --tags
```
`release.yml`: test gate → cross-platform binaries (cli/browser-host/watchdog) →
publish all 7 crates to crates.io in dependency order → GitHub Release. Chrome
extension is zipped and attached.

## VS Code extension — `vscode-v*` (decoupled)

```bash
# bump ONLY the extension when it actually changes:
cd editors/vscode && npm version 0.7.2 --no-git-tag-version && cd ../..
git commit -am "vscode: ..." && git tag vscode-v0.7.2 && git push origin main --tags
```
`release-vscode.yml`: typecheck → build → verify tag==package.json → publish to
Marketplace (+ Open VSX if `OVSX_TOKEN` set) → attach `.vsix` to a GitHub Release.
Run it from the Actions tab with `dry_run=true` to build/package without publishing.

## npm package — `npm-v*` (decoupled)

```bash
cd extensions/npm && npm version 0.7.2 --no-git-tag-version && cd ../..
git commit -am "npm: ..." && git tag npm-v0.7.2 && git push origin main --tags
```
`release-npm.yml`: validate (`npm pack --dry-run`) → `npm publish`.

## Browser extensions (Chrome / Firefox)

Chrome is zipped on the core release. Publishing to the **Chrome Web Store** /
**Firefox AMO** is currently **manual** (upload the zip in each store's dashboard) —
these stores need per-store API credentials + review; automate later if the browser
line is revived (it's currently frozen — see CLAUDE.md / strategy notes).

## Surface Audit (rot detector)

`surface-audit.yml` runs **Mondays 09:00 UTC** (and on demand). It compares the repo
version of each surface against what's actually published (crates.io, npm, VS Code
Marketplace), verifies the VS Code extension still builds, and **fails (emails you)**
if anything drifted. This is what stops a repeat of the 0.2.0-vs-0.7.1 rot.

## Versioning rule

The **core workspace** has one shared version (`bump-version.sh`). The extensions are
**independent** — bump each one's own `package.json`/`manifest.json` only when it
changes. Do not force-bump extensions on a core release (that's what caused rot).
