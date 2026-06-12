// Gauntlet frontend logic. Loaded as an external script so it runs under a
// strict `script-src 'self'` CSP (no inline scripts, no eval). On load it:
//   1. builds a large DOM (stresses dom_snapshot / find_elements / diagnostics),
//   2. floods the IPC layer (stresses the IPC log caps + the ipc-log resource),
//   3. wires deterministic buttons the battery test drives.
//
// All IPC goes through window.__TAURI_INTERNALS__.invoke, which is always
// present regardless of withGlobalTauri.

const GRID_CELLS = 4000; // large DOM: 4000 nodes
const FLOOD_CALLS = 300; // initial IPC flood volume

function invoke(cmd, args) {
  return window.__TAURI_INTERNALS__.invoke(cmd, args || {});
}

function setStatus(text) {
  const el = document.getElementById("status");
  if (el) el.textContent = text;
}

function buildLargeDom() {
  const grid = document.getElementById("grid");
  if (!grid) return;
  const frag = document.createDocumentFragment();
  for (let i = 0; i < GRID_CELLS; i++) {
    const cell = document.createElement("div");
    cell.className = "cell";
    cell.setAttribute("data-idx", String(i));
    cell.textContent = String(i % 100);
    frag.appendChild(cell);
  }
  grid.appendChild(frag);
}

// Fire a burst of real IPC calls. Each `flood_marker` call is a tiny round-trip
// that lands in the bridge's network/IPC log, driving its volume past the
// per-tool caps so we can verify the tools stay bounded (and the ipc-log
// resource doesn't blow the eval result cap and silently truncate).
async function floodIpc(n) {
  const calls = [];
  for (let i = 0; i < n; i++) {
    calls.push(invoke("flood_marker", { seq: i }).catch(() => {}));
  }
  await Promise.all(calls);
}

async function invokeGhost() {
  // A command with NO handler and NOT in the registry. Tauri rejects it with a
  // "not found" error → Victauri must classify it as a confirmed_ghost.
  try {
    await invoke("ghost_command_xyz", {});
  } catch (_e) {
    /* expected: command not found */
  }
}

async function boot() {
  buildLargeDom();
  setStatus("flooding…");
  await floodIpc(FLOOD_CALLS);

  const floodBtn = document.getElementById("flood-btn");
  if (floodBtn) floodBtn.addEventListener("click", () => floodIpc(100));

  const ghostBtn = document.getElementById("ghost-btn");
  if (ghostBtn) ghostBtn.addEventListener("click", invokeGhost);

  const pipelineBtn = document.getElementById("pipeline-btn");
  if (pipelineBtn) pipelineBtn.addEventListener("click", () => invoke("run_pipeline", {}));

  // Mark readiness deterministically so the battery can wait on a stable signal.
  setStatus("gauntlet-ready");
  document.body.setAttribute("data-gauntlet-ready", "1");
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", boot);
} else {
  boot();
}
