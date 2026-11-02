// Records what a real Victauri plugin answers to every request the extension makes, into
// demo-app-responses.json. `fakeServer` replays that file, so the unit suite exercises the real
// response shapes rather than ones invented in a test: a shape nobody recorded is how the App
// State, IPC Log and performance views came to be wrong while every test stayed green.
//
// Re-record after a plugin change that alters a tool's output:
//   1. Build and launch the demo app with a private TEMP/TMP and VICTAURI_PORT.
//   2. node test/fixtures/record.js <port> <token>
// Only ever point this at a demo app you started: it clicks, and it evaluates code in the page.
const fs = require("fs");
const path = require("path");

const [port, token] = process.argv.slice(2);
if (!port || !token) {
  console.error("usage: node record.js <port> <token>");
  process.exit(2);
}
const base = `http://127.0.0.1:${port}`;
const headers = { "content-type": "application/json", authorization: `Bearer ${token}` };

async function tool(name, args = {}) {
  const resp = await fetch(`${base}/api/tools/${name}`, {
    method: "POST",
    headers,
    body: JSON.stringify(args),
  });
  return { status: resp.status, body: await resp.json() };
}

// A 1x1 PNG: the shape of the screenshot answer is real, its megabytes are not worth committing.
const TINY_PNG =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M/AAAMBAQAY3Y2wAAAAAElFTkSuQmCC";

(async () => {
  const info = await (await fetch(`${base}/info`, { headers })).json();

  // Real page IPC, so `logs ipc` holds one success and one failure.
  await tool("eval_js", {
    code: "await window.__TAURI_INTERNALS__.invoke('greet',{name:'fixture'}); return 1",
  });
  await tool("eval_js", {
    code: "try{await window.__TAURI_INTERNALS__.invoke('no_such_command_fixture')}catch(e){}; return 1",
  });
  await new Promise((r) => setTimeout(r, 300));

  const dom = await tool("dom_snapshot", { format: "json" });
  const findRef = (n) => {
    if (n.tag === "button" && n.ref_id) return n.ref_id;
    for (const c of n.children ?? []) {
      const r = findRef(c);
      if (r) return r;
    }
    return undefined;
  };
  const ref = findRef(dom.body.result.tree);

  // Keyed by what fakeServer matches on: the tool, plus its `action` when it has one.
  const tools = {
    "window/list": await tool("window", { action: "list" }),
    "window/get_state": await tool("window", { action: "get_state" }),
    "logs/ipc": await tool("logs", { action: "ipc", limit: 50 }),
    get_memory_stats: await tool("get_memory_stats"),
    get_plugin_info: await tool("get_plugin_info"),
    get_diagnostics: await tool("get_diagnostics"),
    "inspect/get_performance": await tool("inspect", { action: "get_performance" }),
    "inspect/audit_accessibility": await tool("inspect", { action: "audit_accessibility" }),
    "inspect/highlight": await tool("inspect", { action: "highlight", ref_id: ref, color: "#e94560" }),
    "inspect/clear_highlights": await tool("inspect", { action: "clear_highlights" }),
    "inspect/get_styles": await tool("inspect", { action: "get_styles", ref_id: ref }),
    dom_snapshot: dom,
    screenshot: await tool("screenshot"),
    eval_js: await tool("eval_js", { code: "document.title" }),
    "interact/click": await tool("interact", { action: "click", ref_id: ref }),
  };
  if (typeof tools.screenshot.body.result?.data === "string") {
    tools.screenshot.body.result.data = TINY_PNG;
  }

  const out = {
    _note: "Recorded from a real demo app by record.js; do not edit by hand.",
    info,
    // A real failure, whose text the page does not control here.
    click_unknown_ref: await tool("interact", { action: "click", ref_id: "e999999" }),
    tools,
  };
  const file = path.join(__dirname, "demo-app-responses.json");
  fs.writeFileSync(file, JSON.stringify(out, null, 1) + "\n");
  console.log(`wrote ${file}`);
  for (const [k, v] of Object.entries(tools)) {
    console.log(k, v.status, v.body.error ? `ERROR ${v.body.error}` : "");
  }
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
