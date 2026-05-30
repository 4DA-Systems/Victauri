# Agent-Eval Results — A/B PoC (2026-05-31)

**Question:** does Victauri's full-stack visibility make an AI agent better at
debugging a Tauri app than a browser-only tool (CDP/Playwright)?

**Method:** same task, same running demo-app, two agents. **Agent-B** = full
Victauri toolset. **Agent-A** = browser-only (`eval_js` + `dom_snapshot` only;
explicitly barred from `window.__VICTAURI__` to honestly simulate CDP/Playwright).
Each returned a structured rubric. Run: workflow `wf_ba647574-7f8`, 7 agents,
~293k subagent tokens, ~4.8 min. Scored against the answer keys in `tasks.md`
(NOT agent self-report — see the methodology note).

## Results

| Task | Agent-B (full Victauri) | Agent-A (browser-only) |
|---|---|---|
| **T2 — ghost command** (truth lives in the backend IPC registry/log) | ✅ **Correct** — named `ghost_secret_cmd` via `detect_ghost_commands`, corroborated by the IPC log + `get_registry`. **4 calls, read-only, no side effects.** | ❌ **Confidently WRONG** — concluded *"no ghost command exists."* It couldn't see the runtime IPC log, so it invented a workaround: **invoke every frontend command and read the error type.** That (a) reached a false negative, (b) used **8 calls**, and (c) **had destructive side effects** — it actually executed `delete_todo`, `reset_counter`, `send_notification`, `submit_contact`, etc. to "probe." |
| **T6 — control** (pure CSS `pointer-events:none`) | ✅ Correct — 4 calls. | ✅ Correct — 3 calls. |

## What this actually shows (honest reading)

1. **On the backend-invisible bug, full-stack visibility wins on *correctness AND safety*, not just speed.** B answered with a clean, read-only `detect_ghost_commands` query. A — though resourceful — reached a **plausible, confident, WRONG** conclusion, which is *worse than failing* (silent misdiagnosis). And it only got there by **mutating live app state** (invoking real commands as probes), which is unacceptable in real debugging. This is the strongest finding: browser-only doesn't gracefully say "I can't see it" — it confabulates a wrong answer and damages the app trying.

2. **The control (T6) confirms the experiment is fair, not rigged.** For a pure-DOM bug, browser-only is fully sufficient — A even used *fewer* calls (3 vs 4). Victauri offers no advantage where there's nothing backend to see, and we report that honestly. If B had "won" T6 too, the harness would be suspect.

3. **Agents are cleverer than the naive hypothesis assumed.** A *didn't* simply revert — it engineered a probe-by-invoking method. The gap between A and B is therefore not "works vs doesn't"; it's **"correct + safe + cheap vs confidently-wrong + destructive + 2× the calls."** That nuance is *more* credible evidence than a clean sweep.

## Methodology note (important for the full corpus)
- **Do not trust the agent's self-reported `solved`.** A reported `solved=true` on T2 but was objectively wrong. The full harness must score against the **answer key**, not the agent's own verdict — add an independent "judge" agent that grades each answer against ground truth.
- **Caveat on T2 setup:** the ghost command was injected at *runtime* (via eval), which is exactly why A's *static* "inspect the frontend's commands" approach missed it. A real source-level ghost command might be findable by A's probing — so the *generalizable* advantages to emphasize are B's **read-only safety** and **completeness** (the IPC log sees every command actually invoked, incl. dynamic/conditional ones A's static scan can't).

## Next
- Run the full 6-task corpus (`tasks.md`) with an added independent **judge agent** scoring vs the answer keys, and report B-vs-A on `solved` (objective), `tool_calls`, `reverted`, and **side-effects/safety**.
- Add tasks that stress B's *unique* read-only advantages: swallowed IPC error (T3), fault-induced flake (T4), backend-only state (T5).
- Runner: `scripts/.../agent-eval-ab-poc-wf_ba647574-7f8.js` (persisted; extend it).
