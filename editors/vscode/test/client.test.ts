// R5-VSC1: the extension must never keep sending the Bearer token to a port its app no longer
// owns, and its poller must never overlap itself.
import { test } from "node:test";
import assert from "node:assert/strict";

import { VictauriClient } from "../src/client";
import type { DiscoveredServer } from "../src/discovery";
import { fakeServer } from "./fakeServer";
import { stubConfig } from "./vscode-stub";

const entry = (port: number, token: string, identifier: string): DiscoveredServer => ({
  port,
  token,
  identifier,
  pid: 1,
});

test("the token is not sent once discovery no longer vouches for the port", async () => {
  const app = await fakeServer("com.real.app");
  let live = [entry(app.port, "tok-real", "com.real.app")];
  const client = new VictauriClient();
  try {
    stubConfig.pollInterval = 60_000;
    await client.connect(app.port, "tok-real", {
      identifier: "com.real.app",
      discover: async () => live,
    });
    // The app exited (its discovery entry is gone); something else now holds the port.
    live = [];
    const before = app.seen.length;
    await assert.rejects(client.callTool("eval_js", { code: "1" }), /no longer/i);
    assert.equal(app.seen.length, before, "no request (and no token) reached the port");
  } finally {
    client.dispose();
    await app.close();
  }
});

test("a restarted app is followed by identity, verified through /info", async () => {
  const first = await fakeServer("com.real.app");
  const second = await fakeServer("com.real.app");
  let live = [entry(first.port, "tok-1", "com.real.app")];
  const client = new VictauriClient();
  try {
    stubConfig.pollInterval = 60_000;
    await client.connect(first.port, "tok-1", {
      identifier: "com.real.app",
      discover: async () => live,
    });
    live = [entry(second.port, "tok-2", "com.real.app")];
    await client.callTool("get_memory_stats");
    const toolCall = second.seen.find((s) => s.path.startsWith("/api/tools/"));
    assert.equal(toolCall?.auth, "Bearer tok-2");
    assert.ok(
      second.seen.findIndex((s) => s.path === "/info") <
        second.seen.findIndex((s) => s.path.startsWith("/api/tools/")),
      "identity is re-verified before any tool call"
    );
  } finally {
    client.dispose();
    await first.close();
    await second.close();
  }
});

test("a re-resolved endpoint that reports another app is refused", async () => {
  const first = await fakeServer("com.real.app");
  const impostor = await fakeServer("com.someone.else");
  let live = [entry(first.port, "tok-1", "com.real.app")];
  const client = new VictauriClient();
  try {
    stubConfig.pollInterval = 60_000;
    await client.connect(first.port, "tok-1", {
      identifier: "com.real.app",
      discover: async () => live,
    });
    live = [entry(impostor.port, "tok-2", "com.real.app")];
    await assert.rejects(client.callTool("eval_js", { code: "1" }), /com\.someone\.else/);
    assert.equal(
      impostor.seen.filter((s) => s.path.startsWith("/api/tools/")).length,
      0,
      "no tool call reaches an endpoint whose identity does not match"
    );
  } finally {
    client.dispose();
    await first.close();
    await impostor.close();
  }
});

test("the poller never overlaps a slow refresh", async () => {
  const app = await fakeServer("com.slow.app", { infoDelayMs: 700 });
  const client = new VictauriClient();
  try {
    stubConfig.pollInterval = 500; // the minimum
    await client.connect(app.port, "tok");
    const start = app.seen.length;
    let inFlight = 0;
    let maxInFlight = 0;
    const original = client.refreshAll.bind(client);
    client.refreshAll = async () => {
      inFlight++;
      maxInFlight = Math.max(maxInFlight, inFlight);
      try {
        await original();
      } finally {
        inFlight--;
      }
    };
    await new Promise((r) => setTimeout(r, 2_500));
    assert.equal(maxInFlight, 1, "refreshAll ran concurrently with itself");
    assert.ok(app.seen.length > start, "polling still happens");
  } finally {
    client.dispose();
    await app.close();
  }
});

test("/health is unauthenticated, so the token never goes to it", async () => {
  const app = await fakeServer("com.real.app");
  const client = new VictauriClient();
  try {
    stubConfig.pollInterval = 60_000;
    // Discovery vouches for nothing: the connect must fail without the token going anywhere.
    await assert.rejects(
      client.connect(app.port, "SECRET", { identifier: "com.real.app", discover: async () => [] }),
      /no longer/i
    );
    assert.deepEqual(
      app.seen.map((s) => [s.path, s.auth]),
      [["/health", undefined]]
    );
  } finally {
    client.dispose();
    await app.close();
  }
});

test("after a disconnect nothing is sent, and the token is gone", async () => {
  const app = await fakeServer("com.real.app");
  const client = new VictauriClient();
  try {
    stubConfig.pollInterval = 60_000;
    await client.connect(app.port, "tok");
    client.disconnect();
    const before = app.seen.length;
    await assert.rejects(client.callTool("screenshot"), /not connected/i);
    assert.equal(app.seen.length, before, "no request reached the server");
    assert.equal(await client.screenshot().catch(() => "rejected"), "rejected");
    assert.equal(app.seen.length, before);
  } finally {
    client.dispose();
    await app.close();
  }
});

test("a connect after a disconnect works again", async () => {
  const app = await fakeServer("com.real.app");
  const client = new VictauriClient();
  try {
    stubConfig.pollInterval = 60_000;
    await client.connect(app.port, "tok");
    client.disconnect();
    await client.connect(app.port, "tok");
    await client.callTool("screenshot");
    assert.equal(app.seen.at(-1)?.auth, "Bearer tok");
  } finally {
    client.dispose();
    await app.close();
  }
});
