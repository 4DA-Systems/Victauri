# Victauri Security Audit — Triaged Status

**Original audit:** 39 findings, first full pass **2026-05-28**.
**Triaged against `main`:** **2026-05-31** — every finding re-verified at the current HEAD
(file:line evidence checked; the two most consequential verdicts, #30 and #16, re-verified by hand).

> **Read this first — the debug-gating boundary.** `victauri-plugin` is compiled behind
> `#[cfg(debug_assertions)]` and returns a **no-op in release builds**. So every *plugin*
> finding below has a blast radius of **developer machines running debug builds**, not shipped
> end-user software. The findings that reach real users live in the **npm package, the browser
> extension, the VS Code extension, and CI/CD** — those are the priority, and are called out
> explicitly in the "User-facing, still open" list.
>
> Detailed exploit write-ups for the still-open user-facing items are kept in a private working
> copy and are intentionally **not** reproduced here (responsible disclosure while they are
> being remediated). This document tracks *status*, not step-by-step PoCs.

---

## Fixed in this triage pass (2026-05-31)

- **#1 (CRIT, npm) — RESOLVED.** `extensions/npm/scripts/postinstall.js` now pins the SHA-256 of
  every release artifact and **verifies it before the binary is made executable or run**; downloads
  are **HTTPS-only** (redirects to `http://` are refused — the old code chose its client by URL
  prefix and would follow a plaintext redirect); a hash mismatch or missing pin **fails closed**
  (no chmod, no execute). Also corrected the platform→asset name map (the old `darwin-*`/`win32-*`
  names didn't match the published `macos-*`/`windows-*` assets, so non-Linux installs silently 404'd).
- **#11 (LOW, npm) — RESOLVED.** The download `VERSION` is now derived from `package.json`
  (`postinstall.js` and `bin/victauri-browser.js`), so it can no longer drift from the package version.

## Already fixed by the v0.7.2 hardening (verified still-good)

- **#4 (HIGH) — RESOLVED.** Plugin auth is **on by default** now: a UUID Bearer token is auto-generated
  and `require_auth` is applied unless `.auth_disabled()` is called. (The May-28 "auth off by default"
  linchpin no longer holds. Note: a couple of stale doc-comments/tests still describe the old default —
  cosmetic, tracked as cleanup.)
- **#16 (HIGH, codegen RCE) — RESOLVED.** Re-verified by hand: `escape_rust_str` is a correct
  *allowlist* escaper (neutralizes `\ " \n \r \t`; U+2028/2029 are not special in Rust string literals),
  and the real string-literal sink is escaped. The audit's "bypassable denylist" premise was **incorrect**.
  The only raw sink is `//` comment lines, whose inputs (IPC command names from the `ipc.localhost` URL
  path) are structurally newline-free → not exploitable. *Residual (low):* newline-strip the comment path
  for defense-in-depth.
- **#15 write-side — RESOLVED.** The discovery dir/token are written `0700`/`0600` on Unix and locked via
  `icacls` on Windows. (Read-side consumer verification is still open — see below.)
- **#17 — PARTIAL.** A 5 MB eval-output cap + per-entry log truncation exist. Still missing: untrusted-data
  markers around app-sourced tool output, redaction-by-default, and prompt-injection / auto-approve docs.
- **#26 — RESOLVED (by design).** `allow_file_navigation` is opt-in, default off.

---

## ⚠️ User-facing and still open — priority order (NOT debug-gated)

1. **#2 (CRIT) — browser extension bridge has no provenance check.** The MAIN-world bridge accepts
   `__victauri_command` events with no nonce / source check, so a page script can drive it. *Fix:*
   nonce-gate the ISOLATED↔MAIN channel.
2. **#3 (PARTIAL) — browser host:** Host-header check + auth-default now exist, but the Origin guard still
   **fails open when Origin is absent**, and the printed `.mcp.json` carries no token. *Fix:* fail closed on
   missing Origin; include the token in the printed config.
3. **#7 — browser extension permission set** is broad (`debugger`/CDP + `cookies` + `<all_urls>`); tools
   accept any `tab_id`. *Fix:* drop `debugger` (use `captureVisibleTab`), restrict to the active tab.
4. **#13 — browser `navigate`** has no scheme allowlist; installer doesn't validate the extension ID.
5. **#12 — browser host DoS:** unbounded `pending` dispatch map, no concurrency cap.
6. **#9 — VS Code extension** trusts `/tmp/victauri/<pid>/{port,token}` with no ownership check before
   sending the Bearer token. (#15 read-side is the same class in the CLI/test client.)
7. **#8 / #10 / #28 — CI/supply-chain:** several actions still float on mutable tags
   (`dtolnay/rust-toolchain@stable`, `cargo-deny-action@v2`, `install-action@v2`, `peaceiris@v2`);
   the publish job has no `permissions:`/protected `environment:`/tag==version assertion; `deny.toml`
   gates are soft (`yanked = "warn"`, no `vulnerability = "deny"`) and the release gate doesn't run
   `cargo audit`/`cargo-deny`; `cargo install victauri-cli` and the generated CI action ref are unpinned.
8. **#23 — shipped/generated `.mcp.json` templates** are tokenless with no warning, normalizing no-auth.

---

## Full triage table (all 39)

`Reach`: **User** = ships to end users · **Dev** = plugin, debug-gated (dev machines only) · **CI/Dev** = build/dev tooling.

| # | Sev | Area | Reach | Status | Note |
|---|-----|------|-------|--------|------|
| 1 | CRIT | npm | User | ✅ FIXED | pinned SHA-256 + HTTPS-only + fail-closed (this pass) |
| 2 | CRIT | browser-ext | User | 🔴 OPEN | bridge driveable by any page (no nonce) |
| 3 | CRIT | browser-host | User | 🟠 PARTIAL | Host+auth added; Origin fail-open + tokenless config remain |
| 4 | HIGH | plugin | Dev | ✅ RESOLVED | auth on by default (auto token) |
| 5 | HIGH | plugin | Dev | 🔴 OPEN | `app_info` env allowlist still leaks `TAURI_SIGNING_*`/`VICTAURI_AUTH_TOKEN` |
| 6 | HIGH | plugin | Dev | 🔴 OPEN | IPC/network capture unredacted by default |
| 7 | HIGH | browser-ext | User | 🔴 OPEN | broad perms (debugger/cookies/`<all_urls>`), any tab_id |
| 8 | HIGH | CI | CI/Dev | 🟠 PARTIAL | most actions SHA-pinned; some float; publish job unhardened |
| 9 | MED | vscode | User | 🔴 OPEN | trusts temp discovery dir, no ownership check |
| 10 | MED | CI | CI/Dev | 🔴 OPEN | soft `deny.toml`; release gate skips audit/deny |
| 11 | LOW | npm | User | ✅ FIXED | version derived from package.json (this pass) |
| 12 | MED | browser-host | User | 🔴 OPEN | unbounded `pending` map / no backpressure |
| 13 | MED | browser-ext | User | 🔴 OPEN | `navigate` no scheme allowlist; installer no ID validation |
| 14 | HIGH | core | Dev | 🔴 OPEN | `compare_values` no depth guard (stack overflow) |
| 15 | HIGH | cli/test | User | 🟠 PARTIAL | write-side perms hardened; read-side discovery unverified + `remove_dir_all` |
| 16 | HIGH | core | Dev | ✅ RESOLVED | codegen RCE not exploitable (escaper correct); audit premise wrong |
| 17 | HIGH | arch | User | 🟠 PARTIAL | output cap added; no untrusted markers / redaction-default / docs |
| 18 | MED | core | Dev | 🔴 OPEN | `e.index + 1` no `saturating_add` (panic) |
| 19 | MED | core | Dev | 🔴 OPEN | `recording.import` bypasses event/checkpoint caps |
| 20 | MED | core | Dev | 🔴 OPEN | `resolve_command` no query-length cap (CPU DoS) |
| 21 | MED | core | Dev | 🔴 OPEN | `format_element` no depth cap |
| 22 | MED | plugin | Dev | 🔴 OPEN | Wayland `screenshot` captures whole desktop (documented, not cropped) |
| 23 | MED | dist | User | 🔴 OPEN | shipped/generated `.mcp.json` tokenless, no warning |
| 24 | MED | cli | User | 🔴 OPEN | `victauri init` auto-writes CLAUDE.md (no consent, fragile idempotency) |
| 25 | LOW | watchdog | CI/Dev | ⚪ BY-DESIGN | `sh -c $VICTAURI_ON_FAILURE` is an operator-set recovery command |
| 26 | LOW | plugin | Dev | ✅ RESOLVED | file-nav opt-in, default off |
| 27 | LOW | plugin | Dev | ⚪ BY-DESIGN | `inject_css` via `textContent`, agent-only |
| 28 | LOW | CI | CI/Dev | 🔴 OPEN | unpinned `cargo install`; generated CI action ref `@main` |
| 29 | LOW | dev | CI/Dev | 🔴 OPEN | `test_live.sh` predictable world-readable temp file (dev harness) |
| 30 | CRIT | plugin | Dev | 🔴 OPEN | command allow/blocklist bypassed by `replay`/`contract_record`/`contract_check` |
| 31 | CRIT | plugin | Dev | 🔴 OPEN | `recording.import`+`replay` = command invocation from crafted session + cap bypass |
| 32 | HIGH | plugin | Dev | 🔴 OPEN | bridge auto-accepts all `confirm()`/`prompt()` (fail-open) |
| 33 | HIGH | plugin | Dev | 🔴 OPEN | `storage.set` poisons localStorage (allowed even in Test profile) |
| 34 | MED | plugin | Dev | 🔴 OPEN | `fault inject` no TTL / unlimited (scoped to Victauri-driven invokes) |
| 35 | MED | plugin | Dev | 🔴 OPEN | disclosure tools unredacted under default FullControl |
| 36 | LOW | plugin | Dev | ⚪ BY-DESIGN | caller-authored assertions can self-pass (CI trust) |
| 37 | LOW | plugin | Dev | ⚪ ACCEPTABLE | `introspect.processes` leaks child exe names/RSS (no argv) |
| 38 | LOW | plugin | Dev | ⚪ ACCEPTABLE | `window.manage` hide/move/close (FullControl-only) |
| 39 | INFO | plugin | Dev | 🟡 MINOR | `capabilities` mislabels `auth_enabled` (reports redaction); token-leak concern RESOLVED |

**Tally:** 4 fixed (incl. 2 this pass) · 5 already-resolved/by-design · 4 partial · ~23 open
(of which **~7 reach real users**; the rest are debug-gated dev-only).

---

## Highest-value next fixes

**Plugin (dev-only, but the audit's "highest-value defensive fix"):**
- **#30/#31** — call `is_invoke_allowed`/`is_command_allowed` before each direct invoke in
  `recording.replay`, `introspect.contract_record`, `introspect.contract_check`; gate + cap
  `recording.import`/`replay`.
- **#14/#18/#20/#21** — add depth bounds to `compare_values`/`format_element`, `saturating_add` in
  `recording.rs`, and a query-length cap in `resolve_command` (small, contained, well-testable).
- **#32** — default dialogs fail-closed (`confirm → false`, `prompt → null`).

**User-facing (do these for shipped components):** #2, #3, #7, #12, #13 (browser), #9 (VS Code),
#8/#10/#28 (CI), #23 (config templates).

---

## Confirmed strengths — do not regress

Param→JS injection is *not* exploitable in the plugin (`js_string` = `serde_json::to_string`); no DOM-XSS
(`textContent`, not `innerHTML`); plugin `navigate` validates URLs (blocks `javascript:`/`data:`/`file:`);
`sanitize_css_color`; mutex/RwLock poison recovery; capacity caps on `EventLog`/`record_event`/`checkpoint`
(only `import` bypasses — #19); release no-op gate; `127.0.0.1`-only bind; DNS-rebinding + Origin guards on
the plugin; constant-time token compare; rate/body/concurrency limits; security headers; file tools sandboxed
via `safe_within`; DB read-only; password-field redaction in DOM snapshots; `plugin_state`/`get_plugin_info`
do **not** serialize the auth token.
