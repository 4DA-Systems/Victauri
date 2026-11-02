// R4-TOK1 / R4-DISC1 / R4-NET1 for the extension's discovery + connection logic.
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import * as fs from "node:fs";
import * as http from "node:http";
import type { AddressInfo } from "node:net";
import * as os from "node:os";
import * as path from "node:path";

import { authProbeVerdict, healthStatusMeansAlive } from "../src/client";
import {
  DiscoveredServer,
  dirIsTrusted,
  discoveryRoots,
  homeStateRoot,
  ownerCheckEnv,
  pidLiveness,
  resetOwnerCheckBackoff,
  resolveConnection,
  scanServers,
  settlePortClaims,
} from "../src/discovery";

const app = (port: number, token: string, identifier: string, pid = port): DiscoveredServer => ({
  port,
  token,
  identifier,
  pid,
});
const scan = (live: DiscoveredServer[], unverified: number[] = []) => ({ live, unverified });

test("a configured token without a configured port goes only to the app that owns it", () => {
  const live = [app(7373, "not-mine", "com.other"), app(7374, "mine", "com.mine")];
  assert.deepEqual(resolveConnection({ token: "mine" }, scan(live), 7373), {
    ok: true,
    port: 7374,
    token: "mine",
    vouchedBy: live[1],
  });
  const refused = resolveConnection({ token: "mine" }, scan(live.slice(0, 1)), 7373);
  assert.equal(refused.ok, false);
  assert.match(!refused.ok ? refused.message : "", /victauri\.port/);
  // An explicit port WITHOUT a token uses the discovered token, and stays vouched for by
  // that entry (R5-VSC1: the client re-checks discovery before re-sending it).
  assert.deepEqual(resolveConnection({ port: 7374 }, scan(live), 7373), {
    ok: true,
    port: 7374,
    token: "mine",
    vouchedBy: live[1],
  });
  // An explicit port + explicit token keeps the old behaviour.
  assert.deepEqual(resolveConnection({ port: 7373, token: "t" }, scan(live), 7373), {
    ok: true,
    port: 7373,
    token: "t",
  });
});

test("several running apps are an error naming each, never a silent pick", () => {
  const live = [app(7373, "a", "com.a"), app(7374, "b", "com.b")];
  const r = resolveConnection({}, scan(live), 7373);
  assert.equal(r.ok, false);
  const msg = !r.ok ? r.message : "";
  assert.match(msg, /com\.a \(port 7373/);
  assert.match(msg, /com\.b \(port 7374/);
  // One app / none behave as before.
  assert.deepEqual(resolveConnection({}, scan(live.slice(0, 1)), 7373), {
    ok: true,
    port: 7373,
    token: "a",
    vouchedBy: live[0],
  });
  const none = resolveConnection({}, scan([]), 7373);
  assert.deepEqual(none, { ok: true, port: 7373, token: undefined, warning: undefined });
});

test("an unverifiable app is reported, not silently ignored", () => {
  const r = resolveConnection({}, scan([], [4242]), 7373);
  assert.equal(r.ok, true);
  assert.match(r.ok && r.warning ? r.warning : "", /4242/);
});

test("pidLiveness: self is own, an exited child is dead", () => {
  assert.equal(pidLiveness(process.pid), "own");
  assert.equal(pidLiveness(0), "dead");
  const child = spawnSync(process.execPath, ["-e", "0"]);
  assert.equal(pidLiveness(child.pid ?? 0), "dead");
});

test("scanServers never uses (or deletes) an unverified owner's entry", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "victauri-vsc-disc-"));
  try {
    for (const [pid, port] of [
      [101, 7401],
      [102, 7402],
    ]) {
      const dir = path.join(root, String(pid));
      fs.mkdirSync(dir, { mode: 0o700 });
      fs.writeFileSync(path.join(dir, "port"), String(port));
      fs.writeFileSync(path.join(dir, "token"), `tok-${pid}`);
      fs.writeFileSync(
        path.join(dir, "metadata.json"),
        JSON.stringify({ identifier: `com.app${pid}` })
      );
    }
    fs.chmodSync(root, 0o700);
    const result = await scanServers((pid) => (pid === 101 ? "own" : "unverified"), [root]);
    assert.deepEqual(result.live, [
      { port: 7401, token: "tok-101", identifier: "com.app101", pid: 101 },
    ]);
    assert.deepEqual(result.unverified, [102]);
    assert.ok(fs.existsSync(path.join(root, "102")), "entries are never deleted");
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

// A hard-killed app's entry whose PID was reused, naming the port the current app holds: two live
// entries on one port made the extension report "Multiple Victauri apps". As in `victauri-test`.
async function claimsOnOnePort(
  answer: (auth: string | undefined) => { status: number; body?: unknown }
): Promise<{ live: DiscoveredServer[]; asked: number }> {
  let asked = 0;
  const server = http.createServer((req, res) => {
    asked += 1;
    const { status, body } = answer(req.headers.authorization);
    res.writeHead(status, { "content-type": "application/json" });
    res.end(body === undefined ? "" : JSON.stringify(body));
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = (server.address() as AddressInfo).port;
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "victauri-vsc-claims-"));
  try {
    for (const pid of [111, 222]) {
      const dir = path.join(root, String(pid));
      fs.mkdirSync(dir, { mode: 0o700 });
      fs.writeFileSync(path.join(dir, "port"), String(port));
      fs.writeFileSync(path.join(dir, "token"), `tok-${pid}`);
    }
    fs.chmodSync(root, 0o700);
    const { live } = await scanServers(() => "own", [root]);
    return { live, asked };
  } finally {
    server.close();
    fs.rmSync(root, { recursive: true, force: true });
  }
}
const pids = (live: DiscoveredServer[]) => live.map((s) => s.pid).sort();

test("the server on a port says which of several entries is real", async () => {
  const real = (auth: string | undefined) =>
    auth === "Bearer tok-222" ? { status: 200, body: { pid: 222 } } : { status: 401 };
  assert.deepEqual(pids((await claimsOnOnePort(real)).live), [222]);
  // Auth off, or one fixed token: the pid decides.
  const open = () => ({ status: 200, body: { pid: 222 } });
  assert.deepEqual(pids((await claimsOnOnePort(open)).live), [222]);
  // An older plugin reports no pid: accepting the token decides.
  const older = (auth: string | undefined) =>
    auth === "Bearer tok-222" ? { status: 200, body: { name: "victauri" } } : { status: 401 };
  assert.deepEqual(pids((await claimsOnOnePort(older)).live), [222]);
});

test("entries nothing can settle stay, and one claim per port asks nothing", async () => {
  const silent = () => ({ status: 200, body: { name: "victauri" } });
  assert.deepEqual(pids((await claimsOnOnePort(silent)).live), [111, 222]);
  const limited = () => ({ status: 429 });
  assert.deepEqual(pids((await claimsOnOnePort(limited)).live), [111, 222]);

  const root = fs.mkdtempSync(path.join(os.tmpdir(), "victauri-vsc-claims-"));
  try {
    for (const [pid, port] of [
      [111, 1],
      [222, 2],
    ]) {
      const dir = path.join(root, String(pid));
      fs.mkdirSync(dir, { mode: 0o700 });
      fs.writeFileSync(path.join(dir, "port"), String(port));
    }
    fs.chmodSync(root, 0o700);
    const started = Date.now();
    const { live } = await scanServers(() => "own", [root]);
    assert.deepEqual(pids(live), [111, 222]);
    assert.ok(Date.now() - started < 500, "nothing to ask");
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("a rate-limited /health means the server is alive", () => {
  assert.equal(healthStatusMeansAlive(200), true);
  assert.equal(healthStatusMeansAlive(429), true);
  assert.equal(healthStatusMeansAlive(401), false);
  assert.equal(healthStatusMeansAlive(503), false);
});

test("an authenticated /info 429 is not a successful auth probe", () => {
  assert.equal(authProbeVerdict(200), "ok");
  assert.equal(authProbeVerdict(401), "unauthorized");
  assert.equal(authProbeVerdict(429), "rate-limited");
  assert.equal(authProbeVerdict(500), "error");
});

// R5B-WINDISC1: on a shared TEMP (e.g. C:\msys64\tmp) another user can plant
// `victauri\<live pid>\{port,token,…}` pointing at a port they control. On Windows a discovery
// directory is trusted only when the current user (or BUILTIN\Administrators, when this
// token is a member) owns it. SYSTEM stands in for "another user" — setting that up needs an
// elevated run; the test skips otherwise.
test("windows: a discovery directory owned by another account is not trusted", async (t) => {
  if (process.platform !== "win32") {
    t.skip("windows only");
    return;
  }
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "victauri-vsc-owner-"));
  try {
    const mine = path.join(root, "201");
    const planted = path.join(root, "202");
    for (const [dir, port] of [
      [mine, 7501],
      [planted, 7502],
    ] as const) {
      fs.mkdirSync(dir);
      fs.writeFileSync(path.join(dir, "port"), String(port));
      fs.writeFileSync(path.join(dir, "token"), `tok-${port}`);
    }
    const setOwner = spawnSync("icacls", [planted, "/setowner", "*S-1-5-18", "/q"]);
    if (setOwner.status !== 0) {
      t.skip("cannot change a directory's owner (not elevated)");
      return;
    }
    // Force the real owner check even though the test root sits inside the user profile.
    const result = await scanServers((pid) => (pid >= 201 ? "own" : "dead"), [root], {
      profileDir: "Z:\\victauri-no-such-profile",
    });
    assert.deepEqual(
      result.live.map((s) => s.pid),
      [201],
      "a planted entry's token must never be read"
    );
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

// 0.10.2: Windows PowerShell 5.1 launched with PowerShell 7's PSModulePath (VS Code started
// from a pwsh terminal) cannot load `Get-Acl`'s module, so every directory read as not owned.
test("ownerCheckEnv drops PSModulePath, whatever its casing, and passes the paths", () => {
  const env = ownerCheckEnv({ PSModulePath: "C:\\pwsh7\\Modules", psmodulepath: "x", PATH: "p" }, ["C:\\a"]);
  assert.equal(Object.keys(env).some((k) => k.toLowerCase() === "psmodulepath"), false);
  assert.equal(env.PATH, "p");
  assert.equal(env.VICTAURI_OWNER_PATHS, JSON.stringify(["C:\\a"]));
});

test("windows: the owner check works with PowerShell 7's module path inherited", async (t) => {
  if (process.platform !== "win32") {
    t.skip("windows only");
    return;
  }
  const ps7 = path.join(process.env.ProgramFiles || "C:\\Program Files", "PowerShell", "7", "Modules");
  if (!fs.existsSync(ps7)) {
    t.skip("PowerShell 7 is not installed");
    return;
  }
  const saved = process.env.PSModulePath;
  process.env.PSModulePath = [ps7, saved].filter(Boolean).join(";");
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "victauri-vsc-ps7-"));
  try {
    const mine = path.join(root, "301");
    fs.mkdirSync(mine);
    // Outside the (overridden) profile, so the real owner check runs.
    assert.equal(
      await dirIsTrusted(mine, { profileDir: "Z:\\victauri-no-such-profile" }),
      true,
      "a directory this user created must be trusted"
    );
  } finally {
    if (saved === undefined) delete process.env.PSModulePath;
    else process.env.PSModulePath = saved;
    fs.rmSync(root, { recursive: true, force: true });
  }
});

// 0.10.3 (found by CI's first Windows run): a cold Windows PowerShell start passed the old 10 s
// timeout, and the failure was cached — the app's directory stayed untrusted for its lifetime.
test("windows: a failed owner check fails closed but is retried, not cached", async (t) => {
  if (process.platform !== "win32") {
    t.skip("windows only");
    return;
  }
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "victauri-vsc-retry-"));
  const opts = { profileDir: "Z:\\victauri-no-such-profile" };
  try {
    const mine = path.join(root, "401");
    fs.mkdirSync(mine);
    const savedRoot = process.env.SystemRoot;
    process.env.SystemRoot = "Z:\\victauri-no-such-windows"; // PowerShell cannot start
    try {
      assert.equal(await dirIsTrusted(mine, opts), false, "no answer: not trusted");
    } finally {
      process.env.SystemRoot = savedRoot;
    }
    resetOwnerCheckBackoff(); // what a Connect does; otherwise a failure waits out its backoff
    assert.equal(
      await dirIsTrusted(mine, opts),
      true,
      "the failed check must not be cached: a later scan asks again"
    );
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

// R5B-LINDISC1: the plugin falls back to a per-user root under the home directory when the
// predictable `<temp>/victauri-<uid>` was taken over by another user; the extension must scan it.
test("homeStateRoot: $XDG_STATE_HOME/victauri, else ~/.local/state/victauri", () => {
  assert.equal(homeStateRoot({ XDG_STATE_HOME: "/x/state", HOME: "/home/u" }), "/x/state/victauri");
  assert.equal(
    homeStateRoot({ XDG_STATE_HOME: "rel", HOME: "/home/u" }),
    "/home/u/.local/state/victauri"
  );
  assert.equal(homeStateRoot({ HOME: "/home/u" }), "/home/u/.local/state/victauri");
  assert.equal(homeStateRoot({ HOME: "rel" }), undefined);
  assert.equal(homeStateRoot({}), undefined);
});

test("unix: the home fallback root is scanned after <temp>/victauri-<uid>, before the legacy root", async (t) => {
  if (process.platform === "win32") {
    t.skip("unix only");
    return;
  }
  const roots = await discoveryRoots();
  const home = homeStateRoot();
  assert.ok(home, "HOME is set");
  const at = roots.indexOf(home!);
  assert.ok(at > 0 && at < roots.length - 1, JSON.stringify(roots));
  assert.ok(roots[at - 1].endsWith(`victauri-${process.geteuid!()}`), JSON.stringify(roots));
});

// A junction in a parent directory is not visible to a lexical comparison, so "inside the profile"
// must be asked of the real path. SYSTEM stands in for "another user" (elevated runs only).
test("windows: a planted directory reached through a junction inside the profile is not trusted", async (t) => {
  if (process.platform !== "win32") {
    t.skip("windows only");
    return;
  }
  const base = fs.mkdtempSync(path.join(os.tmpdir(), "victauri-vsc-junction-"));
  try {
    const profile = path.join(base, "profile");
    const shared = path.join(base, "shared");
    const planted = path.join(shared, "victauri", "777");
    fs.mkdirSync(profile);
    fs.mkdirSync(planted, { recursive: true });
    if (spawnSync("icacls", [planted, "/setowner", "*S-1-5-18", "/q"]).status !== 0) {
      t.skip("cannot change a directory's owner (not elevated)");
      return;
    }
    fs.symlinkSync(shared, path.join(profile, "Temp"), "junction");
    const viaJunction = path.join(profile, "Temp", "victauri", "777");
    assert.equal(await dirIsTrusted(planted, { profileDir: profile }), false, "direct path");
    assert.equal(await dirIsTrusted(viaJunction, { profileDir: profile }), false, "via junction");
    // A real directory inside the profile still needs no check.
    const inside = path.join(profile, "own");
    fs.mkdirSync(inside);
    assert.equal(await dirIsTrusted(inside, { profileDir: profile }), true);
  } finally {
    fs.rmSync(base, { recursive: true, force: true });
  }
});

test("claims a server refused all stay ambiguous, never 'no app'", async () => {
  const server = http.createServer((_req, res) => {
    res.statusCode = 401;
    res.end();
  });
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
  const port = (server.address() as AddressInfo).port;
  try {
    const stale = app(port, "stale", "com.a", 1);
    const noToken: DiscoveredServer = { port, identifier: "com.a", pid: 2 };
    assert.deepEqual(await settlePortClaims([noToken, stale]), [noToken, stale]);
    const other = app(port, "other", "com.a", 3);
    assert.deepEqual(await settlePortClaims([stale, other]), [stale, other]);
  } finally {
    server.closeAllConnections();
    server.close();
  }
});

test("asking about claims has a total budget across silent ports", async () => {
  const servers: http.Server[] = [];
  const live: DiscoveredServer[] = [];
  for (let i = 0; i < 6; i++) {
    const silent = http.createServer(() => {});
    await new Promise<void>((r) => silent.listen(0, "127.0.0.1", r));
    servers.push(silent);
    const port = (silent.address() as AddressInfo).port;
    live.push(app(port, "a", "com.a", 10 + 2 * i), app(port, "b", "com.a", 11 + 2 * i));
  }
  try {
    const started = Date.now();
    assert.equal((await settlePortClaims(live)).length, live.length);
    assert.ok(Date.now() - started < 3200, `${Date.now() - started} ms`);
  } finally {
    for (const silent of servers) {
      silent.closeAllConnections();
      silent.close();
    }
  }
});