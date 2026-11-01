// The bounds on what one request and one failing owner check can cost.
import { test } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as http from "node:http";
import type { AddressInfo } from "node:net";
import * as os from "node:os";
import * as path from "node:path";

import { VictauriClient, requestLimits } from "../src/client";
import { dirIsTrusted, resetOwnerCheckBackoff, resolveConnection, scanServers } from "../src/discovery";
import {
  identifierFromTauriConf,
  pollIntervalMs,
  readConnectionSettings,
  validPort,
} from "../src/settings";
import { neutralizeLinks } from "../src/toast";
import { stubConfig } from "./vscode-stub";

/** Answers /health and /info, then lets `tool` decide how to answer a tool call. */
async function server(tool: (res: http.ServerResponse) => void) {
  const srv = http.createServer((req, res) => {
    if (req.url === "/health" || req.url === "/info") {
      res.writeHead(200, { "content-type": "application/json" });
      return res.end("{}");
    }
    tool(res);
  });
  await new Promise<void>((r) => srv.listen(0, "127.0.0.1", r));
  return {
    port: (srv.address() as AddressInfo).port,
    close: () =>
      new Promise<void>((r) => {
        srv.closeAllConnections();
        srv.close(() => r());
      }),
  };
}

async function withLimits(
  limits: Partial<typeof requestLimits>,
  body: () => Promise<void>
): Promise<void> {
  const saved = { ...requestLimits };
  Object.assign(requestLimits, limits);
  try {
    await body();
  } finally {
    Object.assign(requestLimits, saved);
  }
}

test("a stalled body hits the request timeout instead of hanging forever", async () => {
  const srv = await server((res) => {
    res.writeHead(200, { "content-type": "application/json" });
    res.write('{"result":'); // and never finishes
  });
  const client = new VictauriClient();
  try {
    stubConfig.pollInterval = 60_000;
    await withLimits({ timeoutMs: 300 }, async () => {
      await client.connect(srv.port);
      const started = Date.now();
      await assert.rejects(client.callTool("x"), /did not answer/);
      assert.ok(Date.now() - started < 5_000);
    });
  } finally {
    client.dispose();
    await srv.close();
  }
});

test("an oversized answer is refused", async () => {
  const srv = await server((res) => {
    res.writeHead(200, { "content-type": "application/json" });
    res.end("x".repeat(4096));
  });
  const client = new VictauriClient();
  try {
    stubConfig.pollInterval = 60_000;
    await withLimits({ maxResponseBytes: 1024 }, async () => {
      await client.connect(srv.port);
      await assert.rejects(client.callTool("x"), /larger than 1024 bytes/);
    });
  } finally {
    client.dispose();
    await srv.close();
  }
});

test("neutralizeLinks leaves nothing VS Code's parser can link", () => {
  const link = /\[([^\]]+)\]\(((?:https?:\/\/|command:|file:)[^)\s]+)(?: (["'])(.+?)(\3))?\)/gi;
  for (const s of [
    "[a](command:x)",
    "[a](https://e.example)",
    "[a](file:///c:/x)",
    "]([a](command:x)",
    "[a]](command:x)",
    "[a](command:x 'title')",
    "[A](COMMAND:x)",
  ]) {
    // VS Code turns line breaks into spaces before parsing: that cannot rejoin a link.
    const shown = neutralizeLinks(s).replace(/(\r\n|\n|\r)/gm, " ");
    assert.equal(shown.match(link), null, s);
  }
  assert.equal(neutralizeLinks("no links (here) [at all]"), "no links (here) [at all]");
});

test("settings: values of the wrong type are refused", () => {
  for (const bad of ["7373", 0, -1, 65536, 1.5, null, {}, "80@evil.example"]) {
    assert.equal(validPort(bad), undefined, String(bad));
  }
  assert.equal(validPort(7373), 7373);
  assert.equal(pollIntervalMs(0), 500);
  assert.equal(pollIntervalMs(-5), 500);
  assert.equal(pollIntervalMs("abc"), 2000);
  assert.equal(pollIntervalMs(NaN), 2000);
  assert.equal(pollIntervalMs(undefined), 2000);
  assert.equal(pollIntervalMs(5000), 5000);
  assert.equal(pollIntervalMs(1e15), 3_600_000, "beyond 2^31 ms a timer fires after 1 ms");
});

test("settings: only a user-level token is read, and a workspace port is flagged", () => {
  const source = (v: Record<string, Record<string, unknown>>) => ({ inspect: (k: string) => v[k] });
  const both = readConnectionSettings(
    source({ port: { workspaceValue: 7400 }, authToken: { globalValue: " tok ", workspaceValue: "repo" } })
  );
  assert.deepEqual(
    { port: both.port, fromWs: both.portFromWorkspace, token: both.token },
    { port: 7400, fromWs: true, token: "tok" }
  );
  const repoToken = readConnectionSettings(source({ authToken: { workspaceValue: "repo" } }));
  assert.equal(repoToken.token, undefined, "a repository cannot supply the token");
  const user = readConnectionSettings(source({ port: { globalValue: 7400 } }));
  assert.equal(user.portFromWorkspace, false);
  const bad = readConnectionSettings(source({ port: { workspaceValue: "x" } }));
  assert.deepEqual([bad.port, bad.invalidPort, bad.portFromWorkspace], [undefined, "x", false]);
});

test("settings: the identifier is read from each Tauri config format", () => {
  assert.equal(identifierFromTauriConf("a/tauri.conf.json", '{"identifier":"com.a"}'), "com.a");
  assert.equal(
    identifierFromTauriConf("tauri.conf.json", '{"tauri":{"bundle":{"identifier":"com.v1"}}}'),
    "com.v1"
  );
  assert.equal(identifierFromTauriConf("tauri.conf.json", "{}"), undefined);
  assert.equal(identifierFromTauriConf("tauri.conf.json", "not json"), undefined);
  assert.equal(identifierFromTauriConf("tauri.conf.json5", "{\n  identifier: 'com.j5',\n}"), "com.j5");
  assert.equal(identifierFromTauriConf("Tauri.toml", 'identifier = "com.toml"'), "com.toml");
});

test("resolveConnection: a failed owner check is the reported cause, not a 401 later", () => {
  const r = resolveConnection({}, { live: [], unverified: [], ownerCheckFailed: true }, 7373);
  assert.equal(r.ok, false);
  assert.match(!r.ok ? r.message : "", /could not be checked/);
});

test("windows: a broken owner check costs at most one query per backoff", async (t) => {
  if (process.platform !== "win32") {
    t.skip("windows only");
    return;
  }
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "victauri-vsc-backoff-"));
  try {
    const dir = path.join(root, "501");
    fs.mkdirSync(dir);
    let queries = 0;
    let verdict: boolean[] | null = null;
    const opts = {
      profileDir: "Z:\\victauri-no-such-profile",
      queryOwners: async () => {
        queries++;
        await new Promise((r) => setTimeout(r, 30));
        return verdict;
      },
    };
    // Many requests scanning at once share one query.
    const results = await Promise.all(Array.from({ length: 9 }, () => dirIsTrusted(dir, opts)));
    assert.deepEqual(results, Array(9).fill(false));
    assert.equal(queries, 1, "concurrent scans share one query");
    // Later polls while it is backing off start nothing.
    for (let i = 0; i < 5; i++) assert.equal(await dirIsTrusted(dir, opts), false);
    assert.equal(queries, 1, "no query during the backoff");
    // The verdict is still not cached: after the backoff, one answer settles it.
    resetOwnerCheckBackoff();
    verdict = [true];
    assert.equal(await dirIsTrusted(dir, opts), true);
    assert.equal(queries, 2);
    assert.equal(await dirIsTrusted(dir, opts), true);
    assert.equal(queries, 2, "a definite answer is cached");
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("windows: a scan whose owner check fails says so", async (t) => {
  if (process.platform !== "win32") {
    t.skip("windows only");
    return;
  }
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "victauri-vsc-flag-"));
  try {
    const result = await scanServers(() => "own", [root], {
      profileDir: "Z:\\victauri-no-such-profile",
      queryOwners: async () => null,
    });
    assert.equal(result.ownerCheckFailed, true);
    assert.deepEqual(result.live, []);
  } finally {
    resetOwnerCheckBackoff();
    fs.rmSync(root, { recursive: true, force: true });
  }
});
