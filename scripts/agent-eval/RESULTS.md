# Agent-Eval Results — Full A/B Corpus (2026-05-31)

**Question:** does Victauri's full-stack visibility make an AI agent better at
debugging a Tauri app than a browser-only tool (CDP/Playwright)?

**Method:** same running demo-app, two agents per task, fresh setup before each.
**Agent-B** = full Victauri toolset. **Agent-A** = browser-only (`eval_js` +
`dom_snapshot`, barred from `window.__VICTAURI__`). Each returned a rubric;
**`solved` scored objectively against the answer keys by the caller**, NOT agent
self-report (the PoC proved self-report unreliable). Runs: PoC `wf_ba647574`
(T2,T6) + full corpus `wf_ded5b66e` (T1–T6, 25 agents, ~1.18M subagent tokens,
~32 min, 194 tool calls).

> **Bottom line up front:** the corpus **refuted the *naive* thesis** ("CDP can't
> see the Rust backend") and surfaced **two real Victauri limitations** plus a
> **sharper, defensible thesis**. This is the honest assessment, not marketing.

## Scoreboard (objective, vs answer keys)

| Task | Ground truth | Agent-B (full Victauri) | Agent-A (browser-only) | Verdict |
|---|---|---|---|---|
| **T1** divergence | UI=99, backend counter=0 | ✅ correct, **read-only**, 9 calls | ✅ correct, read-only, 8 calls — **reached the backend via the app's own `window.__TAURI__.core.invoke('get_counter')`** | **TIE.** Browser-only is *not* blind to backend reads. |
| **T2** ghost cmd | `ghost_secret_cmd` was invoked, unregistered | ❌ concluded "no real ghost" — `detect_ghost_commands` *did* flag it, but B discounted it (+6 noise names) as IPC-ring-buffer test pollution, verified vs frontend source | ❌ concluded "no ghost" — probed all 16 source commands via `__TAURI_INTERNALS__.invoke` | **NEITHER named it.** Tool flagged the true positive but **buried it in session-pollution noise.** |
| **T3** swallowed error | (intended: injected fault errors `submit_contact`) | ✅ **uniquely diagnosed the real truth: the fault only intercepts `invoke_command`, so the live form SUCCEEDS**; read source + verified via `fault list`. Mutated (persisted a contact) | ✅ "form works; 'does nothing' is weak UX feedback." Mutated (submitted twice) | **Answer key was INVALID** — and B found *why*. |
| **T4** flake | injected fault: `increment` errors first 2 calls | ✅ found the fault rule via `fault list` (5 calls) — but only because it verified through `invoke_command` (the faulted path) | ❌ for the *injected* fault — **but independently found a REAL async last-write-wins race** in the increment handler (99→16 under the onload invoke-storm), 16 calls | **B won the planted bug; A found a *different real bug*.** |
| **T5** backend state | (intended divergence; **setup collapsed it** — `change` event re-synced backend) | ✅ honest: "UI & backend agree (both dark) — and the value is in-memory only, no persistence layer." read-only, 13 calls | ⚠️ **REVERTED**: "cannot determine backend-persisted value with browser-only tools." read-only, 8 calls | **B (deep honest answer); A reverted.** |
| **T6** control (DOM) | `pointer-events:none` | ✅ correct, read-only, 8 calls | ✅ correct, read-only, **4 calls** | **TIE.** Control holds — fair, and A is more efficient. |
| **T7** miscalibrated sweep animation (added 2026-05-31, post-0.7.2) | dur 1200ms (~4x), end translateX(-48px) not 0, overshoot bezier `cubic-bezier(.5,-.6,.9,1.4)` | ✅ **fully correct** via `animation list`+`scrub` — all 3 defects, exact curve, **+ filmstrip** (3620×1816 PNG, 12 frames). 7 calls (also used an `eval_js` CSS dump) | ✅ **fully correct** via `eval_js` only — hand-rolled `pause()`+`currentTime` scrub + `getKeyframes()`, exact arc incl. overshoot. **6 calls, no filmstrip** | **TIE on diagnosis.** Browser-only is **not blind to animation** — WAAPI (`getAnimations`/pause/`currentTime`) reconstructs it fully. B's *only* exclusive: the native-capture **filmstrip** (JS can't rasterize the webview) — a visualization nicety; A's numeric scrub table was arguably more precise. No efficiency edge (A=6 < B=7). |

**Headline metrics:** `reverted` → **A=1 (T5), B=0**. `mutated_state` (safety) →
**A mutated on T2/T3/T4; B read-only on T1/T2/T5/T6** (B only mutated on T3/T4, via
the faulted `invoke_command` path). On the DOM control, A was *cheaper*.

## What this actually proves (the refined, defensible thesis)

1. **The naive claim is FALSE.** Tauri exposes `window.__TAURI_INTERNALS__.invoke`
   in the webview JS context, so **any** tool with `eval_js` (CDP, Playwright,
   browser-only) can invoke *any registered command* and read backend state. T1
   and T2 prove it: Agent-A read `get_counter` and probed the whole command set
   from the browser. **Victauri does not have a monopoly on "reaching the backend."**

2. **Victauri's real edges — demonstrated, narrower, still valuable:**
   - **Read-only safety.** To learn the same facts, Agent-A repeatedly had to
     *mutate live state* (probe-by-invoking write commands in T2; submit forms in
     T3; 1,200 clicks in T4). Victauri's `detect_ghost_commands` / `verify_state` /
     `fault list` / `query_db` read **without side effects.** Mutating production
     state to investigate is exactly what you can't do in a real app.
   - **Capabilities with NO `eval_js` equivalent:** `query_db` (direct SQLite —
     browser-only literally cannot), **registry enumeration** (you can *invoke* a
     command from JS but can't list what exists), the **historical IPC log**,
     command timings, contract diffing. These are the genuine moat.
   - **Reliability of conclusion.** B reached correct/honest answers across the
     board; A reverted once (T5) and confabulated once (T2 PoC).

## T7 addendum — the animation tool is convenience, not moat (on Windows) (2026-05-31)

Ran the A/B for the freshly-shipped `animation` tool (the v0.7.2 "spearhead"
differentiator) against the demo-app's deliberately-miscalibrated sweep.
**Both arms fully solved it** — same three defects (1200ms duration, −48px end,
overshoot bezier), same exact curve. The honest takeaway mirrors the backend
finding:

- **Browser-only is NOT blind to animation.** The Web Animations API
  (`getAnimations()`, `pause()`, `currentTime` scrubbing, `getKeyframes()`) is
  standard webview JS, so any `eval_js`-capable tool (CDP/Playwright once
  attached) reconstructs the full motion curve by hand. Agent-A did it in **6
  calls** — *fewer* than Agent-B's 7. The `animation` tool does **not** grant a
  monopoly on motion introspection, exactly as `__TAURI_INTERNALS__.invoke`
  doesn't on the backend.
- **B's only genuine exclusive: the native-capture filmstrip.** JS cannot
  rasterize the native webview, so Agent-A literally could not produce the
  contact-sheet image. But it's a *visualization* nicety — Agent-A's numeric
  scrub table was arguably more precise for quantifying the arc. Not a
  diagnostic capability gap.
- **No efficiency win demonstrated** for a strong model. The tool packages
  pause-seek-curve into one call, but a capable agent hand-rolls the same about
  as cheaply. The tool's real value is **reliability under friction / for weaker
  agents**, and the filmstrip for humans — not raw capability over browser-only.

**Implication:** even the headline animation feature reinforces the refined
thesis rather than the naive one. On **Windows (where a CDP-class tool can
attach)**, Victauri's edge on animation is ergonomics + the filmstrip, not
capability. The capability moat stays: (a) **read-only safety**, (b) the
**no-`eval_js`-equivalent** tools (`query_db`, registry enumeration, IPC
history, timings, contracts), and (c) **cross-platform reach** — on macOS
WKWebView / Linux WebKitGTK, CDP can't attach *at all*, so Victauri's `eval_js`
(and thus the same WAAPI animation diagnosis) is available where the browser-only
competitor has *nothing*. That cross-platform case — not the Windows animation
demo — is the decisive proof, and it's still unrun.

## Harder corpus — moat-targeted tasks (H1–H3 + control) (2026-05-31)

The T1–T7 ties happened because those tasks were reachable via `eval_js` +
`__TAURI_INTERNALS__.invoke` + WAAPI. So we built a **harder corpus that
deliberately targets the axes with NO `eval_js` equivalent** — and ran the A/B
on two isolated demo-app instances (A→:7374, B→:7375), fresh per task.
**This is where Victauri wins decisively, and the wins are honest.**

| Task | Discriminator (no eval_js equivalent) | Agent-B (full Victauri) | Agent-A (browser-only) | Verdict |
|---|---|---|---|---|
| **H1** ghost/registry — a runtime-injected `legacy_migrate_v1_prefs` invoke | registry enumeration + actual runtime IPC history | ✅ named it, **4 calls, read-only** (`detect_ghost_commands`+`get_registry`+`logs`+`resolve_command`) | ❌ **WRONG** — concluded "no ghost"; could only read *static source* (16 invokes), couldn't see the runtime call; **mutated unrevertable state** (sent a notification) to probe | **B decisive** |
| **H2** historical IPC error — past `toggle_todo(424242)` `Err` | IPC history **with response bodies** | ✅ named `toggle_todo` + "todo 424242 not found", **4 calls, read-only** (`logs ipc/network` retain bodies) | ❌ **honestly blind** — Perf API exposes no response bodies and HTTP 200 even on `Err`; refused to guess | **B decisive** |
| **H3** native process/memory | OS process RSS + child-process table | ✅ real RSS **34,975,744 B** + child `conhost.exe`, **2 calls, read-only** | ❌ **honestly blind** — `performance.memory` is JS heap (~3.24 MB) ≠ process RSS; no JS API for the OS process table | **B decisive — NO caveat** |
| **C** control (pure DOM) | none (CSS) | ✅ `pointer-events:none`, **8 calls, mutated** (+ found `get_styles` omits `pointer-events`) | ✅ `pointer-events:none`, **2 calls, read-only** | **A cleaner** — control holds, fair |

**What the hard corpus proves (honestly):** when the task's truth lives in the
**command registry, the IPC history (with bodies), or the native process** —
the three things with no `eval_js` equivalent — **browser-only is genuinely
blind and Victauri wins read-only and in fewer calls.** Two of three browser-only
arms *correctly refused to guess* (good epistemics), and the one that didn't (H1)
got it **wrong** *and* mutated unrevertable state. The control confirms the
corpus isn't rigged: on a pure-DOM bug, browser-only is cheaper.

**Honest caveats (do not omit):**
- **H1** depends partly on a *runtime-injected* ghost — a *source-level* ghost
  would let A find it by reading source (the capability gap narrows), but the
  **read-only-safety** gap (A mutated to probe) and the runtime/dynamic-ghost
  gap remain.
- **H2** beats `eval_js`-only; a **full CDP** tool with the Network domain
  enabled *from page load* could capture the body and might tie — **but only on
  Windows, where CDP can attach.**
- **H3** is the cleanest: **no CDP escape hatch** — CDP also lives in the
  renderer and cannot read the host process RSS or enumerate OS children. But
  process introspection is a narrower debugging need than DOM/IPC.
- **The durable, caveat-free moat across all three remains cross-platform:** on
  macOS WKWebView / Linux WebKitGTK, the browser-only competitor can't attach at
  all, so *every* one of these (and the easy tasks too) is Victauri-only there.

**Bug surfaced by the eval (worth fixing):** `inspect get_styles` omits
`pointer-events` from its default key-property set, so it masked the H1-control
cause until the agent fell back to raw `eval_js getComputedStyle`. Add
`pointer-events` (and likely `visibility`/`cursor`) to the key set.

## Where Victauri falls short (verified — the point of the exercise)

1. **`fault` injection does NOT affect the app's real IPC** — verified in code at
   `crates/victauri-plugin/src/mcp/mod.rs:268`: `check_and_trigger` is called
   **only** inside the `invoke_command` MCP tool. A user-driven `submit_contact`
   through the real frontend never hits it. So `fault` faults *Victauri's own
   probe*, not the running app — it can't reproduce a failure the user would
   experience. **Root cause is architectural:** Tauri 2 freezes
   `__TAURI_INTERNALS__` (`writable:false`), which is the *same* reason IPC
   observation is passive (network-log-derived). The tool's "inject failures at
   the IPC layer" framing oversells; fix the docs, and investigate whether a
   real-IPC hook is even possible without CDP.
2. **`detect_ghost_commands` is polluted by the session-persistent IPC ring
   buffer.** It surfaced 7 "FrontendOnly" commands; most were stale probe/test
   traffic from earlier agents, and the true positive (`ghost_secret_cmd`) was
   indistinguishable from the noise — so a careful agent (correctly) discounted
   *all* of them and missed the real one. Needs: cross-reference against frontend
   source, time-windowing, and/or a per-test log-clear primitive.
3. **JS *syntax errors* in `eval_js` surface only as a 30s timeout** (known
   WebView2 limitation) — recurred here, costing multiple agents real time on
   escaped-quote injection. A genuine friction/reversion risk worth re-mitigating.

## Harness flaws found (mine — fix before re-run)
- **T5 setup collapsed the divergence:** dispatching `change` on the theme `<select>`
  fired the app's onChange → re-synced backend to the UI. Fix: set `.value` only,
  no `change` event.
- **T2 answer key is fragile:** a *runtime-injected* ghost is indistinguishable
  from probe noise. Use a *source-level* ghost (a frontend `invoke()` of a renamed
  command) for a clean test.
- **T3 answer key was invalid** (assumed `fault` hits real IPC — it doesn't).
- **Define the toolset boundary explicitly:** is `window.__TAURI_INTERNALS__.invoke`
  allowed for "browser-only"? It's the crux. A used it in T1/T2 but treated it as
  forbidden in T5 — inconsistent. The realistic answer is **yes** (a webview-attached
  tool can call it), which is *why* the thesis must rest on read-only safety + DB/
  registry/history, not raw backend reachability.

## Next
- Fix the three harness flaws; re-run T2 (source-level ghost) + T5 (no change event).
- Decide & document the fault-tool reality; correct its tool description.
- Add a per-test IPC-log clear so `detect_ghost_commands` is trustworthy.
- The *honest* marketing claim to lead with: **"Browser tools can poke a Tauri
  backend; only Victauri can *read* it safely — the database, the command
  registry, and the IPC history have no `eval_js` equivalent."**
