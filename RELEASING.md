# Releasing Victauri

Each surface ships **independently**, and a weekly **Surface Audit** fails loudly if any
published version falls behind the repo.

**Two paths in this repo are written but have never run:**

- **Crates:** `release.yml`'s Trusted Publishing steps. Every release so far published the six crates
  locally first, so the workflow skipped them. The local publish below is the proven path.
- **VS Code extension:** `release-vscode.yml`'s publish job. The extension ships by manual `.vsix` upload.

## Required GitHub secrets (Settings → Secrets and variables → Actions)

None is needed for the paths in use. These are for the unproven ones.

| Secret | Used by | Scope |
|---|---|---|
| *(none for crates.io)* | core release → crates.io | **Trusted Publishing (OIDC), written since 0.10.1 and never exercised:** nothing confirms that the six crates have the publisher configured, and no preflight checks it. The `publish` job exchanges a GitHub OIDC token for a ~30-minute crates.io token (`rust-lang/crates-io-auth-action`, revoked when the job ends). One-time setup, by a crate owner, for **each of the six crates** on crates.io: crate → Settings → Trusted Publishing → Add → GitHub, owner `4DA-Systems`, repository `Victauri`, workflow `release.yml`, environment `release`. Configure **all six before tagging**: the token covers only the crates configured, so a missing one fails the publish at that crate after the earlier ones are live, and `github-release` (which needs `publish`) is skipped until the config is fixed and the job re-run (published crates are skipped). If Trusted Publishing is unavailable, publishing all six crates locally first still works: when every crate is already on crates.io at the tag's version the workflow skips authentication and publishing and goes on to the GitHub Release. Optionally then enable "trusted publishing only" on each crate and delete any old `CARGO_REGISTRY_TOKEN` secret — the workflow no longer reads it. |
| `VSCE_PAT` | `release-vscode.yml` → Marketplace (not set; the workflow's publish would fail) | Azure DevOps PAT, *Marketplace → Manage* (publisher `4da-systems`) |
| `OVSX_TOKEN` *(optional)* | `release-vscode.yml` → Open VSX (not set) | open-vsx.org token (Cursor / VSCodium / agent IDEs) |

> **Browser mode was removed (2026-06-09).** The `victauri-browser` crate, the Chrome/Firefox
> extensions, and the `@4da/victauri-browser` npm package are gone (crates.io yanked, npm
> unpublished). Victauri is Tauri-only. The `NPM_TOKEN` secret and the browser/npm release
> tracks below no longer exist.

## Before you tag — the pre-publish gate

0.7.4 shipped with two bugs (a macOS-only discovery bug and a real-app E2E failure) because
the release's own test step runs **only on ubuntu** and publish wasn't gated on the full CI.
Two layers now prevent that:

1. **Local preflight** — run `./scripts/preflight.ps1` (Windows) or `./scripts/preflight.sh`
   before pushing. It runs fmt + clippy + the doc-count lint + `npm ci` for the jsdom JS-bridge
   tests + the full workspace tests (with `VICTAURI_REQUIRE_JSDOM=1`, so the bridge tests fail
   rather than silently skip), so fast failures are caught locally in seconds instead of a
   16-minute CI round trip. It
   **cannot** catch macOS/Linux-only bugs (you're on one OS) or the real-app E2E — that's CI.
2. **`require-ci-green` (the hard gate)** — `release.yml` will **not** publish unless the
   tagged commit is **on `main`** and the **full CI push run on `main`** (all platforms +
   real-app E2E) concluded **success** for that exact commit. (A pull-request run tested the
   merge of the commit into `main`, not the commit itself, so it does not count.) A tag on a
   non-CI-green commit is refused, loudly, before the publish step.
3. **Semver report (local)** — `cargo semver-checks check-release --workspace` before bumping.
   CI runs the same check but only as information; read its output rather than its colour.
   **For a 0.x major bump (e.g. 0.8.x → 0.9.0) that command checks nothing**: a major bump
   already permits every lint, so it prints "0 checks … N skip" and passes. Also run
   `cargo semver-checks check-release --workspace --release-type minor` (or per crate with
   `-p`) and read the failures: they are the list of what actually changed, and every item in it
   must be covered by MIGRATION.md.
4. **Publish runs verified-then-tokenless** — `release.yml` first runs
   `cargo publish --workspace --dry-run` with no token present, then uploads each crate with
   `--no-verify` in a step that alone holds the token, so no build script ever runs next to it.
5. **No caches in `release.yml`** — a tag run reads main's cache scope, and `compat.yml` runs
   third-party build scripts in it, so a restored entry could reach the shipped binaries.
   `scripts/check-release-no-cache.sh` (in CI) fails if a cache comes back.

**Recommended flow:** `preflight` → push to `main` → **wait for CI green on all platforms** →
bump + tag. (Pushing commit + tag together still works — the gate waits for CI to finish, up
to ~40 min, then requires success.)

## Core (crates + binaries) — `vMAJOR.MINOR.PATCH`

The tag pattern is `v[0-9]+.[0-9]+.[0-9]+`, so it never matches a `vscode-v*` tag. The `release-tags`
ruleset blocks moving or deleting `v*` and `vscode-v*` tags, so a wrong tag cannot be repointed: cut a new
version. Creating one is allowed.

**The proven path, used for every release so far: publish locally, then tag.** Crates cannot be
unpublished, so wait for main's CI on the merge commit first, and use a clean worktree at that commit.

```bash
./scripts/bump-version.sh 0.8.0     # updates the workspace version everywhere
# edit CHANGELOG.md, merge to main, wait for CI green on the merge commit. Then, at that commit:
cargo publish --workspace --dry-run --locked     # with no token in the environment
CARGO_REGISTRY_TOKEN=<operator token> cargo publish --workspace --no-verify --locked
git tag v0.8.0 && git push origin v0.8.0
```

`release.yml` then runs: test gate → cross-platform binaries (cli/watchdog) → require CI green →
verify packages → publish (every crate is already live at the tag's version, so authentication and
upload are skipped) → GitHub Release.

The publish job still waits for approval in the `release` environment, even with nothing to upload.
"Require CI green" fails if main's CI has not finished green: rerun the failed jobs once it has.

**Trusted Publishing is unproven.** Tagging without a local publish would run that path for the first
time. Do it only on a release you are ready to finish by hand: a crate without the publisher configured
fails at its own turn, after the earlier crates are irreversibly live.

Then list the release in the **MCP Registry** (`server.json`, name
`io.github.4DA-Systems/victauri` — the namespace is the GitHub login and is matched
case-sensitively): run the **MCP Registry** workflow (`gh workflow run mcp-registry.yml`, from `main`: it refuses any other
ref) once the crates are on crates.io. It logs in with the workflow's GitHub OIDC token (no secret, no
interactive login), checks that server.json's version is published and its description fits the
schema (≤ 100 characters), and runs a pinned, checksum-verified `mcp-publisher publish`. (By hand:
`mcp-publisher login github && mcp-publisher publish`.) The registry reads the `mcp-name:` line
from the README of the *published* `victauri-cli` version, so it must be right before the crate
is uploaded.

## VS Code extension (decoupled)

**The real flow is a manual `.vsix` upload.** Hand a `.vsix` over only after its commit is merged and
main's full CI is green, including the Windows extension leg (`VS Code extension (windows-latest)`).
Build it from that commit:

```bash
cd editors/vscode
npm version 0.7.2 --no-git-tag-version    # bump ONLY the extension, when it actually changes
# commit, merge, wait for CI green. Then, at the merge commit:
npm ci --ignore-scripts --no-audit --no-fund
npm run lint
npm run build
npm test
npx --no-install vsce package --no-dependencies -o victauri-0.7.2.vsix
```

Upload it at `marketplace.visualstudio.com/manage/publishers/4da-systems` → Victauri.

**`release-vscode.yml` has never published.** Its `dry_run=true` mode gives you a CI-built `.vsix` from any
branch. A `vscode-v*` tag would run: typecheck → build → verify tag==package.json → require CI green for
the commit (main's push run, plus the Windows leg by name) → publish to Marketplace (+ Open VSX if
`OVSX_TOKEN` set) → attach the `.vsix` to a GitHub Release. It stays unused until `VSCE_PAT` is set in the
`release` environment, and a tag pushed without it fails at the publish step.

## Surface Audit (rot detector)

`surface-audit.yml` runs **Mondays 09:00 UTC** (and on demand). It compares the repo
version of each surface against what's actually published (crates.io, VS Code
Marketplace, MCP Registry, GitHub Release binaries), verifies the VS Code extension still builds, and **fails (emails you)**
if anything drifted. This is what stops a repeat of the 0.2.0-vs-0.7.1 rot.

## Versioning rule

The **core workspace** has one shared version (`bump-version.sh`). The VS Code extension
is **independent** — bump its own `package.json` only when it changes. Do not force-bump
the extension on a core release (that's what caused rot).
