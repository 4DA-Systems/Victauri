// R4-TOK1 / R4-DISC1 / R4-NET1 for the extension's discovery + connection logic.
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

import { authProbeVerdict, healthStatusMeansAlive } from "../src/client";
import {
  DiscoveredServer,
  pidLiveness,
  resolveConnection,
  scanServers,
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
  });
  const refused = resolveConnection({ token: "mine" }, scan(live.slice(0, 1)), 7373);
  assert.equal(refused.ok, false);
  assert.match(!refused.ok ? refused.message : "", /victauri\.port/);
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
