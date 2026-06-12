// Secondary-window logic. Fires IPC from a NON-default window so the battery
// can prove the multi-window drain captures it (a single-window drain would be
// blind to everything here).
function invoke(cmd, args) {
  return window.__TAURI_INTERNALS__.invoke(cmd, args || {});
}

function boot() {
  const btn = document.getElementById("sec-emit");
  if (btn) btn.addEventListener("click", () => invoke("flood_marker", { seq: -1 }));
  // Console output here also exercises cross-window console capture.
  console.log("[secondary] ready");
  document.body.setAttribute("data-secondary-ready", "1");
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", boot);
} else {
  boot();
}
