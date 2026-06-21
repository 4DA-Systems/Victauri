# The macOS Wedge — verified, with sources

**Claim:** On macOS, external Tauri automation tools cannot attach to the WKWebView
at all, and the tools that *can* run there (new embedded WebDrivers) are DOM-only.
**Victauri is the sole full-stack option on macOS** — DOM + IPC + Rust backend + DB
+ native, through one MCP interface.

This is the strongest, most defensible part of Victauri's positioning, because it is
*positional* (where the competition structurally can't go), not a capability one
opponent can replicate out-of-process.

## Verified facts (not assertion)

1. **The official Tauri WebDriver path does not run on macOS.** Tauri's own docs:
   > "On desktop, only Windows and Linux are supported due to macOS not having a
   > WKWebView driver tool available."
   Apple ships no WebDriver for WKWebView, so Selenium / WebdriverIO via
   `tauri-driver` cannot test a macOS Tauri app. ([Tauri WebDriver docs](https://v2.tauri.app/develop/tests/webdriver/), [tauri#7068](https://github.com/tauri-apps/tauri/issues/7068))

2. **WebDriver is DOM-only — the whole competitor category.** Tauri's docs describe
   WebDriver as "a standardized interface to interact with web documents" — its scope
   is the webview DOM. It does **not** cover the Rust backend, IPC, or the database.
   ([Tauri WebDriver docs](https://v2.tauri.app/develop/tests/webdriver/))

3. **External CDP/Playwright can't attach to WKWebView either.** WKWebView exposes
   WebKit's own remote inspector, not CDP; Playwright drives its *own* WebKit build,
   not an embedded WKWebView in a third-party app. There is no external-attach path
   on macOS.

4. **Honest nuance (this corrected my first draft):** as of early 2026 there ARE
   community **embedded** WebDriver servers for macOS Tauri — e.g.
   [danielraffel/tauri-webdriver](https://github.com/danielraffel/tauri-webdriver)
   (W3C WebDriver v1 for WKWebView) and
   [Choochmeque/tauri-webdriver](https://github.com/Choochmeque/tauri-webdriver)
   (cross-platform, embedded). They use Victauri's *architecture* (server inside the
   app), so "nothing works on macOS" is no longer true. **But they are WebDriver —
   DOM-only by protocol.** None give IPC / Rust backend / database / native
   introspection. So even where macOS DOM automation now exists, Victauri remains the
   only full-stack option — and the only MCP-native one.

## Victauri's macOS readiness (code audit, this repo)

| Capability | macOS status |
|---|---|
| MCP/REST server, `eval_js`, `dom_snapshot`, `find_elements` | ✅ platform-independent (Tauri JS-bridge injection) |
| `invoke_command` (IPC → Rust), `get_registry`, `verify_state`, `query_db` | ✅ platform-independent (in-process `AppHandle`) |
| `screenshot` | ✅ implemented (`CGWindowListCreateImage`) — but needs Screen-Recording TCC grant at runtime |
| native window handle | ✅ implemented (`ns_view` → `windowNumber`) |
| `get_memory_stats` | ✅ implemented (`task_info` / `MACH_TASK_BASIC_INFO` → `resident_bytes`) |
| child-process enumeration | ✅ implemented (`proc_listchildpids`) |
| **trusted (`isTrusted:true`) input** | ⚠️ **stubbed** on macOS — falls back to synthetic events (CGEvent impl pending) |

**Honest gap:** the workspace compiles and unit-tests green on macOS in CI, but a
*live Tauri app driven by Victauri on real Apple hardware* had not been demonstrated
end-to-end until the CI job below. Native input is the one true feature gap.

## The proof (real Apple hardware, reproducible)

CI job **`macOS Full-Stack Proof`** (`.github/workflows/ci.yml`) launches the demo
Tauri app on a `macos-latest` runner and asserts, live, the five layers no macOS
automation tool can reach together:

1. **webview** — `eval_js` `6*7` → `42` inside the WKWebView
2. **dom** — `dom_snapshot` returns ref handles
3. **ipc → rust backend** — `invoke_command get_counter` returns a real value
4. **backend** — `get_registry` enumerates the command surface
5. **native** — `get_memory_stats` returns real `resident_bytes`

Screenshot and trusted input are deliberately excluded: they need
Screen-Recording / Accessibility TCC grants headless CI can't give. The
same-process introspection that *is* the wedge needs none of that.

**Result: PROVEN GREEN on a real 3-platform matrix** — the `fullstack-proof` job
runs on `macos-latest`, `ubuntu-latest`, and `windows-latest`. All three green
(run [26701699830](https://github.com/4DA-Systems/victauri/actions/runs/26701699830);
macOS first proven solo on run 26690831417 and green on every run since). The step
runs under `set -euo pipefail` with a `fail()` that exits non-zero on any missed
assertion, so green deterministically means: the Tauri app launched, the embedded
MCP server came up, the webview became eval-able, and webview-eval (`6*7→42`) + DOM
snapshot + IPC→Rust-backend invoke + registry enumeration + native `resident_bytes`
all succeeded — the four layers below the glass, live, on every desktop platform.
Re-runs on every push to `main`.

macOS is the strategically decisive cell (no external tool can attach there at all);
Linux and Windows make it a cross-platform guarantee rather than a one-platform
claim. (Windows initially failed because the axum server binds before WebView2
finishes its cold init — fixed with a webview-readiness poll before asserting; it
was a CI cold-start race, not a capability gap. `eval` works on real Windows.)

## Live cloud-Mac verification (2026-06-01, Scaleway M2 / macOS Tahoe 26.3)

Re-validated current `main` on a real Apple-silicon box (not CI), incl. all code
added since the last Mac run (animation suite, `window introspectability`,
`blank_frame_reason`, agent-eval era):

- **Build + clippy clean; full `cargo test --workspace` green** on arm64 macOS 26.3.
  (Caveat: `deep_adversarial_tests` false-fails on a fresh Mac due to macOS's
  default `ulimit -n` = 256 — the concurrent-server battery exhausts FDs with
  "Too many open files"; `ulimit -n 8192` → **107/0 passed**. Not a code bug;
  worth adding `ulimit -n 8192` to `macos-deep-test.sh`.)
- **Backend introspection works HEADLESS (no GUI/Aqua session):** launched the
  demo-app over plain SSH with no window server; the WKWebView never rendered
  (so `eval_js` returns empty — no bridge), **but the embedded MCP server came up
  and `get_memory_stats` returned real macOS process RSS (73,170,944 B) and
  `get_registry` returned 19 commands.** The "full-stack ≠ webview-dependent"
  property, live on macOS: with the webview entirely absent, Victauri still
  answers from direct `AppHandle` access — while a browser-only / CDP tool on
  macOS has *nothing* (can't attach to WKWebView, and here there's no rendered
  webview either). Mirrors the live-4DA finding (query_db worked while the webview
  bridge was down).
- **The rendered-webview 4-layer proof is already CI-green** on macOS (above).
- **`animation` tool verified live on a real macOS WKWebView (2026-06-02).** A
  GUI/Aqua session was established **over SSH, no VNC client**: FileVault is off,
  so set `autoLoginUser=m1` + write `/etc/kcpassword`, then `sudo killall
  loginwindow` → m1 auto-logs into Aqua (no reboot); launch the app with
  `sudo launchctl asuser 501 sudo -u m1 <binary>` to enter the GUI bootstrap
  context. Result on the live WKWebView: `eval_js 6*7→42`, and **all three
  `animation` actions** — `list` (WAAPI exact config on WebKit), `scrub`
  (geometry curve identical shape to Windows: tx 420→469→…→-48), `sample`
  (74 frames, 1211 ms, 0 jank) — **plus a REAL filmstrip** (2716×1816, 8 frames,
  ~400 KB PNG; a blank would be <20 KB). `CGWindowListCreateImage` captured the
  WKWebView content with **no Screen-Recording/TCC grant needed for own-window
  capture** on Tahoe 26.3, and it captures composited content — so the filmstrip
  works on macOS where Windows GDI fails on transparent windows. **Net: on the
  one platform where CDP/Playwright cannot attach at all, Victauri's *entire*
  suite — webview + IPC + backend + DB + native + animation/filmstrip — works.**

## The honest one-liner

> On macOS, the blessed Tauri E2E tooling doesn't run, and the new embedded drivers
> see only the DOM. Victauri is the one tool that gives an agent the DOM **and** the
> IPC, the Rust backend, the database, and native state — on the platform where
> everything else stops at the glass (or stops at the door).

Sources: [Tauri WebDriver docs](https://v2.tauri.app/develop/tests/webdriver/) ·
[tauri#7068 (macOS support request)](https://github.com/tauri-apps/tauri/issues/7068) ·
[danielraffel/tauri-webdriver](https://github.com/danielraffel/tauri-webdriver) ·
[Choochmeque/tauri-webdriver](https://github.com/Choochmeque/tauri-webdriver)
