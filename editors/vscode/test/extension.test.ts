// The real `activate()`: every command, run through the stubbed VS Code API against a fake server
// answering with recorded real responses. Discovery reads a private TEMP built here, so a test
// can never see, let alone talk to, an app really running on this machine.
import { after, afterEach, before, beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { ChildProcess, spawn } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

import { activate, deactivate } from "../src/extension";
import { FakeServer, fakeServer } from "./fakeServer";
import {
  commands,
  openedDocuments,
  panels,
  shownMessages,
  statusBar,
  stubAnswers,
  stubConfig,
  stubCounters,
  stubWorkspaceConfig,
  stubWorkspaceFiles,
} from "./vscode-stub";

// VS Code's own link grammar (vs/base/common/linkedText.ts), read from the shipped bundle.
const VSCODE_LINK = /\[([^\]]+)\]\(((?:https?:\/\/|command:|file:)[^)\s]+)(?: (["'])(.+?)(\3))?\)/gi;
const HOSTILE = "[Retry](command:workbench.action.terminal.sendSequence?%7B%22text%22%3A%22x%22%7D)";

const savedEnv = { ...process.env };
let root: string;
let discovery: string;
const sleepers: ChildProcess[] = [];
const servers: FakeServer[] = [];
let context: {
  subscriptions: Array<{ dispose: () => unknown }>;
  workspaceState: { get: (k: string) => unknown; update: (k: string, v: unknown) => Promise<void> };
};

before(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), "victauri-vsc-ext-"));
  fs.chmodSync(root, 0o700);
  process.env.TMP = root;
  process.env.TEMP = root;
  process.env.TMPDIR = root;
  process.env.HOME = root;
  delete process.env.XDG_RUNTIME_DIR;
  delete process.env.XDG_STATE_HOME;
  discovery = path.join(root, "victauri");
});

after(() => {
  process.env = savedEnv;
  fs.rmSync(root, { recursive: true, force: true });
});

beforeEach(() => {
  fs.rmSync(discovery, { recursive: true, force: true });
  fs.mkdirSync(discovery, { mode: 0o700 });
  shownMessages.length = 0;
  openedDocuments.length = 0;
  panels.length = 0;
  stubWorkspaceFiles.length = 0;
  stubAnswers.warning = undefined;
  stubAnswers.input = undefined;
  for (const o of [stubConfig, stubWorkspaceConfig]) {
    for (const k of Object.keys(o)) delete o[k];
  }
  stubConfig.pollInterval = 60_000;
  stubConfig.autoConnect = false;
  const state = new Map<string, unknown>();
  context = {
    subscriptions: [],
    workspaceState: {
      get: (k) => state.get(k),
      update: async (k, v) => void state.set(k, v),
    },
  };
});

afterEach(async () => {
  for (const s of context.subscriptions) s.dispose();
  for (const p of panels) p.close?.();
  deactivate();
  for (const s of servers.splice(0)) await s.close();
  for (const c of sleepers.splice(0)) c.kill();
});

/** A discovery entry for a live process: this one, or a new sleeper when this pid is taken. */
function advertise(port: number, token: string, identifier: string): void {
  let pid = process.pid;
  if (fs.existsSync(path.join(discovery, String(pid)))) {
    const child = spawn(process.execPath, ["-e", "setTimeout(()=>{},120000)"], { stdio: "ignore" });
    sleepers.push(child);
    pid = child.pid ?? 0;
  }
  const dir = path.join(discovery, String(pid));
  fs.mkdirSync(dir, { mode: 0o700 });
  fs.writeFileSync(path.join(dir, "port"), String(port));
  fs.writeFileSync(path.join(dir, "token"), token);
  fs.writeFileSync(path.join(dir, "metadata.json"), JSON.stringify({ identifier }));
}

async function serve(identifier = "com.example.victauri-demo", override = {}): Promise<FakeServer> {
  const s = await fakeServer(identifier, { override });
  servers.push(s);
  return s;
}

function start(): void {
  activate(context as never);
}

const run = (id: string, ...args: unknown[]) => commands.executeCommand(id, ...args);

const toasts = () => shownMessages.map((m) => m.text);

async function until(what: string, cond: () => boolean, ms = 9_000): Promise<void> {
  const end = Date.now() + ms;
  while (!cond()) {
    assert.ok(Date.now() < end, `timed out waiting for ${what}; toasts: ${toasts().join(" | ")}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}

const BUTTON = { ref_id: "e5", tag: "button" };

const FAILING_COMMANDS: Array<[string, string, unknown[]]> = [
  ["victauri.evalJs", "eval_js", []],
  ["victauri.smokeTest", "get_diagnostics", []],
  ["victauri.clickElement", "interact/click", [BUTTON]],
  ["victauri.highlightElement", "inspect/highlight", [BUTTON]],
  ["victauri.clearHighlights", "inspect/clear_highlights", []],
  ["victauri.inspectStyles", "inspect/get_styles", [BUTTON]],
  ["victauri.auditA11y", "inspect/audit_accessibility", []],
  ["victauri.perfMetrics", "inspect/get_performance", []],
];

for (const [command, tool, args] of FAILING_COMMANDS) {
  test(`${command}: page text in an error cannot become a command link`, async () => {
    const app = await serve("com.example.victauri-demo", {
      [tool]: { body: { error: `boom ${HOSTILE}` } },
    });
    advertise(app.port, "tok", "com.example.victauri-demo");
    stubAnswers.input = "document.title";
    start();
    await run("victauri.connect");
    shownMessages.length = 0;
    await run(command, ...args);
    const text = toasts().join("\n");
    assert.match(text, /boom/, "the failure is reported");
    assert.match(text, /command:workbench/, "the text stays readable");
    assert.equal(text.match(VSCODE_LINK), null, text);
  });
}

test("an app identifier in a refusal cannot become a command link", async () => {
  const app = await serve(HOSTILE);
  advertise(app.port, "tok", "com.real.app");
  start();
  await run("victauri.connect");
  const text = toasts().join("\n");
  assert.match(text, /answers as/);
  assert.equal(text.match(VSCODE_LINK), null, text);
});

test("a lost-connection warning cannot carry a command link", async () => {
  const app = await serve();
  advertise(app.port, "tok", "com.example.victauri-demo");
  stubConfig.pollInterval = 500;
  start();
  await run("victauri.connect");
  // The port changed hands: the next poll hears another app's name, chosen by that app.
  app.override["/info"] = HOSTILE;
  await until("the lost-connection toast", () => toasts().some((t) => /Lost connection/.test(t)));
  const text = toasts().join("\n");
  assert.match(text, /command:workbench/, "the app's text is what is being tested");
  assert.equal(text.match(VSCODE_LINK), null, text);
});

test("no toast call in src/ can bypass the neutralising helper", () => {
  const src = path.join(process.cwd(), "src");
  const offenders: string[] = [];
  for (const f of fs.readdirSync(src)) {
    if (f === "toast.ts") continue;
    const text = fs.readFileSync(path.join(src, f), "utf-8");
    if (/show(Error|Warning|Information)Message/.test(text)) offenders.push(f);
  }
  assert.deepEqual(offenders, [], "use toast.ts: VS Code renders [x](command:y) in toasts as links");
  const toast = fs.readFileSync(path.join(src, "toast.ts"), "utf-8");
  for (const m of toast.matchAll(/window\.show(?:Error|Warning|Information)Message\(([^,)]*)/g)) {
    assert.match(m[1], /neutralizeLinks\(/, `unneutralised toast: ${m[0]}`);
  }
});

test("a workspace-chosen port does not get the user's token without consent", async () => {
  const app = await serve();
  stubConfig.authToken = "USER-LEVEL-SECRET";
  stubWorkspaceConfig.port = app.port;
  start();
  stubAnswers.warning = undefined; // dismissed
  await run("victauri.connect");
  assert.ok(app.seen.length > 0, "the port was contacted");
  assert.deepEqual(
    app.seen.filter((s) => s.auth),
    [],
    "no request carried the token"
  );
  assert.ok(toasts().some((t) => /workspace/i.test(t)), "the user was asked");
});

test("the user can consent to a workspace-chosen port, once for that port", async () => {
  const app = await serve();
  stubConfig.authToken = "USER-LEVEL-SECRET";
  stubWorkspaceConfig.port = app.port;
  start();
  const consent = "Send the token";
  stubAnswers.warning = consent;
  await run("victauri.connect");
  assert.ok(app.seen.some((s) => s.auth === "Bearer USER-LEVEL-SECRET"));
  shownMessages.length = 0;
  stubAnswers.warning = undefined;
  await run("victauri.connect");
  assert.ok(!toasts().some((t) => /workspace/i.test(t)), "not asked twice for the same port");
});

test("auto-connect never prompts, and never sends the token to a workspace port", async () => {
  const app = await serve();
  stubConfig.authToken = "USER-LEVEL-SECRET";
  stubConfig.autoConnect = true;
  stubWorkspaceConfig.port = app.port;
  start();
  await until("the auto-connect", () => app.seen.length > 0);
  await until("the data load", () => app.seen.some((s) => s.path.startsWith("/api/tools/")));
  assert.deepEqual(app.seen.filter((s) => s.auth), []);
  assert.ok(!shownMessages.some((m) => m.items.length > 0), "no prompt while auto-connecting");
});

test("a user-level port with a user-level token needs no consent", async () => {
  const app = await serve();
  stubConfig.authToken = "USER-LEVEL-SECRET";
  stubConfig.port = app.port;
  start();
  await run("victauri.connect");
  assert.ok(app.seen.some((s) => s.auth === "Bearer USER-LEVEL-SECRET"));
  assert.ok(!toasts().some((t) => /workspace/i.test(t)));
});

test("a workspace can only poison the poll interval as far as the minimum", async () => {
  for (const bad of [0, "abc", -5]) {
    const app = await serve();
    advertise(app.port, "tok", "com.example.victauri-demo");
    stubWorkspaceConfig.pollInterval = bad;
    start();
    await run("victauri.connect");
    const before = app.seen.length;
    await new Promise((r) => setTimeout(r, 1_200));
    // The minimum interval is 500 ms and one poll here is 8-9 requests.
    const made = app.seen.length - before;
    assert.ok(made <= 3 * 9, `pollInterval=${String(bad)}: ${made} requests in 1.2 s`);
    deactivate();
    fs.rmSync(path.join(discovery, String(process.pid)), { recursive: true, force: true });
  }
});

test("an invalid port setting is refused, not used", async () => {
  const app = await serve();
  advertise(app.port, "tok", "com.example.victauri-demo");
  for (const bad of ["80@evil.example", 0, 70_000, 1.5, HOSTILE]) {
    shownMessages.length = 0;
    stubWorkspaceConfig.port = bad;
    start();
    await run("victauri.connect");
    const text = toasts().join("\n");
    assert.match(text, /victauri\.port/, String(bad));
    assert.equal(text.match(VSCODE_LINK), null, text);
    assert.ok(
      new RegExp(`com\\.example\\.victauri-demo on port ${app.port}`).test(text),
      `falls back to discovery for ${String(bad)}: ${text}`
    );
    deactivate();
  }
});

test("context-menu commands run from the palette without an argument do not throw", async () => {
  const app = await serve();
  advertise(app.port, "tok", "com.example.victauri-demo");
  start();
  await run("victauri.connect");
  for (const id of [
    "victauri.generateTest",
    "victauri.generateTestForCommand",
    "victauri.clickElement",
    "victauri.highlightElement",
    "victauri.inspectStyles",
    "victauri.copyRefId",
  ]) {
    await assert.doesNotReject(async () => run(id), id);
  }
  assert.deepEqual(openedDocuments, [], "no document for no element");
});

test("the success toast and the status bar name the app", async () => {
  const app = await serve();
  advertise(app.port, "tok", "com.example.victauri-demo");
  start();
  await run("victauri.connect");
  assert.ok(
    toasts().some((t) => t.includes(`com.example.victauri-demo on port ${app.port}`)),
    toasts().join("|")
  );
  assert.match(String(statusBar.tooltip), /com\.example\.victauri-demo/);
});

function workspaceConf(body: unknown): void {
  const file = path.join(root, `conf-${stubWorkspaceFiles.length}.json`);
  fs.writeFileSync(file, JSON.stringify(body));
  stubWorkspaceFiles.push(file);
}

test("several apps running: the workspace's own app is chosen", async () => {
  const other = await serve("com.other.app");
  const mine = await serve("com.mine.app");
  advertise(other.port, "tok-o", "com.other.app");
  advertise(mine.port, "tok-m", "com.mine.app");
  workspaceConf({ identifier: "com.mine.app" });
  start();
  await run("victauri.connect");
  assert.ok(toasts().some((t) => t.includes(`com.mine.app on port ${mine.port}`)), toasts().join("|"));
  assert.ok(!other.seen.some((s) => s.auth), "the other app never saw a token");
});

test("only another app running: a manual connect warns, auto-connect stays away", async () => {
  const other = await serve("com.other.app");
  advertise(other.port, "tok-o", "com.other.app");
  workspaceConf({ identifier: "com.mine.app" });
  stubConfig.autoConnect = true;
  start();
  await new Promise((r) => setTimeout(r, 400));
  assert.deepEqual(other.seen, [], "auto-connect must not bind to another project's app");
  await run("victauri.connect");
  const text = toasts().join("\n");
  assert.match(text, /com\.mine\.app/);
  assert.match(text, /com\.other\.app/);
});

test("another project's app is reported once, and the workspace is not searched every retry", async () => {
  const other = await serve("com.other.app");
  advertise(other.port, "tok-o", "com.other.app");
  workspaceConf({ identifier: "com.mine.app" });
  stubConfig.autoConnect = true;
  stubCounters.findFiles = 0;
  start();
  await new Promise((r) => setTimeout(r, 11_000)); // the first try and two retries
  const said = toasts().filter((t) => t.includes("com.other.app"));
  assert.equal(said.length, 1, toasts().join(" | "));
  assert.match(said[0], /com\.mine\.app/);
  assert.equal(stubCounters.findFiles, 1, "the search is not repeated on every retry");
  assert.deepEqual(other.seen, [], "still stays away from it");
  await run("victauri.connect");
  assert.equal(stubCounters.findFiles, 2, "a deliberate Connect looks again");
});

test("one failed connect does not poison every later one", async () => {
  const app = await serve();
  advertise(app.port, "tok", "com.example.victauri-demo");
  stubConfig.port = app.port;
  stubConfig.authToken = "tok";
  stubWorkspaceConfig.port = app.port;
  stubAnswers.warning = "Send the token";
  start();
  const update = context.workspaceState.update;
  context.workspaceState.update = async () => {
    throw new Error("storage is gone");
  };
  await assert.rejects(Promise.resolve(run("victauri.connect")), /storage is gone/);
  context.workspaceState.update = update;
  await run("victauri.connect");
  assert.equal(statusBar.command, "victauri.disconnect", "the second connect ran");
});

test("Disconnect during a connect wins, and no one is told it connected", async () => {
  const s = await fakeServer("com.example.victauri-demo", { infoDelayMs: 300 });
  servers.push(s);
  advertise(s.port, "tok", "com.example.victauri-demo");
  start();
  const connecting = run("victauri.connect");
  await until("the probe", () => s.seen.some((x) => x.path === "/info"));
  await run("victauri.disconnect");
  await connecting;
  await new Promise((r) => setTimeout(r, 100));
  assert.equal(statusBar.command, "victauri.connect", "it stays disconnected");
  assert.ok(!toasts().some((t) => /Connected/.test(t)), toasts().join(" | "));
});

test("auto-connect finds an app started after VS Code, with no error toast", async () => {
  stubConfig.autoConnect = true;
  start();
  await new Promise((r) => setTimeout(r, 300));
  assert.deepEqual(toasts(), [], "nothing running yet is not an error");
  const app = await serve();
  advertise(app.port, "tok", "com.example.victauri-demo");
  await until("the retry to connect", () => app.seen.some((s) => s.auth === "Bearer tok"));
  await until("the toast", () => toasts().some((t) => t.includes("com.example.victauri-demo")));
});

test("after an explicit Disconnect auto-connect does not reconnect", async () => {
  const app = await serve();
  advertise(app.port, "tok", "com.example.victauri-demo");
  stubConfig.autoConnect = true;
  start();
  await until("the auto-connect", () => statusBar.command === "victauri.disconnect");
  await run("victauri.disconnect");
  await new Promise((r) => setTimeout(r, 300)); // requests already on the wire finish
  const before = app.seen.length;
  await new Promise((r) => setTimeout(r, 6_000));
  assert.equal(app.seen.length, before, "stayed disconnected");
  assert.equal(statusBar.command, "victauri.connect");
});

test("a screenshot answer that is not base64 is not rendered as HTML", async () => {
  const payload = `"><h1 id=injected>INJECTED</h1><a href="https://evil.example">Save As...</a><img src="data:,`;
  const app = await serve("com.example.victauri-demo", {
    screenshot: { body: { result: { type: "image", data: payload } } },
  });
  advertise(app.port, "tok", "com.example.victauri-demo");
  start();
  await run("victauri.connect");
  await run("victauri.screenshot");
  await until("the panel", () => panels.length > 0 && panels[0].webview.html !== "");
  const html = panels[0].webview.html;
  assert.doesNotMatch(html, /<h1/);
  assert.doesNotMatch(html, /evil\.example/);
});

test("a real screenshot renders; the error page's styles obey its own CSP", async () => {
  const app = await serve();
  advertise(app.port, "tok", "com.example.victauri-demo");
  start();
  await run("victauri.connect");
  await run("victauri.screenshot");
  await until("the panel", () => panels.length > 0 && panels[0].webview.html !== "");
  assert.match(panels[0].webview.html, /<img src="data:image\/png;base64,iVBOR/);

  await run("victauri.disconnect");
  await panels[0].fireMessage?.({ command: "refresh" });
  const html = panels[0].webview.html;
  assert.match(html, /Screenshot failed/);
  assert.doesNotMatch(html, /style="/, "style-src allows nonce'd <style> only");
  assert.match(html, /<style nonce="/);
});

test("the manifest hides argument-taking commands and declares its limits", () => {
  const pkg = JSON.parse(fs.readFileSync(path.join(process.cwd(), "package.json"), "utf-8"));
  const palette: Array<{ command: string; when: string }> = pkg.contributes.menus.commandPalette;
  for (const id of [
    "victauri.copyRefId",
    "victauri.generateTest",
    "victauri.generateTestForCommand",
    "victauri.clickElement",
    "victauri.highlightElement",
    "victauri.inspectStyles",
  ]) {
    assert.equal(palette.find((p) => p.command === id)?.when, "false", id);
  }
  const props = pkg.contributes.configuration.properties;
  assert.equal(props["victauri.authToken"].scope, "machine");
  assert.equal(props["victauri.pollInterval"].minimum, 500);
  assert.equal(props["victauri.port"].minimum, 1);
  assert.equal(props["victauri.port"].maximum, 65535);
  assert.equal(pkg.capabilities.untrustedWorkspaces.supported, false);
  assert.equal(pkg.capabilities.virtualWorkspaces, false);
  assert.equal(pkg.scripts["vscode:prepublish"], "npm run build");
  for (const e of ["tauri.conf.json", "tauri.conf.json5", "Tauri.toml"]) {
    assert.ok(pkg.activationEvents.includes(`workspaceContains:**/${e}`), e);
  }
  const icon = pkg.contributes.viewsContainers.activitybar[0].icon;
  assert.match(icon, /\.svg$/, "VS Code masks this icon, so an opaque PNG is a blank square");
  assert.doesNotMatch(fs.readFileSync(path.join(process.cwd(), icon), "utf-8"), /<rect[^>]*fill/);
  const ignored = fs.readFileSync(path.join(process.cwd(), ".vscodeignore"), "utf-8");
  assert.match(ignored, /media\/icon-\*\.png/);
});
