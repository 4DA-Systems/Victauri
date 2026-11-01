// The views against what a real plugin answers (fixtures/demo-app-responses.json). Fake servers
// that answered `{"result":{}}` to everything are why these views could be wrong while the
// suite stayed green.
import { test } from "node:test";
import assert from "node:assert/strict";

import { AppStateProvider } from "../src/appStateView";
import { VictauriClient } from "../src/client";
import { DomExplorerProvider } from "../src/domExplorerView";
import { IpcLogProvider } from "../src/ipcLogView";
import { FakeServerOptions, fakeServer } from "./fakeServer";
import { stubConfig } from "./vscode-stub";

async function connected(options: FakeServerOptions = {}) {
  stubConfig.pollInterval = 60_000;
  const app = await fakeServer("com.example.victauri-demo", options);
  const client = new VictauriClient();
  await client.connect(app.port);
  return {
    app,
    client,
    done: async () => {
      client.dispose();
      await app.close();
    },
  };
}

function ipcRow(client: VictauriClient, command: string) {
  const ipc = new IpcLogProvider(client);
  const entry = ipc.getChildren().find((e) => e.command === command);
  assert.ok(entry, `no IPC row for ${command}`);
  const item = ipc.getTreeItem(entry);
  return { item, icon: (item.iconPath as { id: string }).id };
}

test("every request a refresh makes was answered by a real plugin", async () => {
  const { app, done } = await connected();
  try {
    assert.deepEqual(app.unrecorded, []);
  } finally {
    await done();
  }
});

test("App State shows the app's windows from the real window answer", async () => {
  const { client, done } = await connected();
  try {
    const nodes = new AppStateProvider(client).getChildren();
    const windows = nodes.find((n) => n.label === "Windows");
    assert.ok(windows, `no Windows node: ${JSON.stringify(nodes.map((n) => n.label))}`);
    assert.equal(windows.children?.[0]?.label, "main");
    assert.equal(windows.children?.[0]?.description, "900x600");
    for (const label of ["Memory", "Plugin", "Performance"]) {
      assert.ok(nodes.some((n) => n.label === label), `missing ${label}`);
    }
  } finally {
    await done();
  }
});

test("one odd window entry does not take the App State tree down", async () => {
  const good = {
    label: "good",
    title: "t",
    url: "u",
    visible: true,
    focused: true,
    size: [10, 20],
    position: [0, 0],
  };
  const { client, done } = await connected({
    override: { "window/get_state": { body: { result: [{ label: "odd" }, null, good] } } },
  });
  try {
    const nodes = new AppStateProvider(client).getChildren();
    const windows = nodes.find((n) => n.label === "Windows");
    assert.deepEqual(
      windows?.children?.map((c) => c.label),
      ["good"]
    );
  } finally {
    await done();
  }
});

test("Performance reads the DOM and long-task figures the tool really returns", async () => {
  const { client, done } = await connected();
  try {
    const perf = new AppStateProvider(client).getChildren().find((n) => n.label === "Performance");
    const row = (label: string) => perf?.children?.find((c) => c.label === label)?.description;
    assert.equal(row("DOM Elements"), "97 (depth 5)");
    assert.equal(row("Long Tasks"), "0");
    assert.ok(row("JS Heap"), "JS Heap row");
  } finally {
    await done();
  }
});

test("Long Tasks has no row on an engine without the API", async () => {
  const { client, done } = await connected({
    override: {
      "inspect/get_performance": {
        body: {
          result: {
            dom: { elements: 1, max_depth: 1 },
            js_heap: { unavailable: true },
            long_tasks: { unavailable: true, reason: "Long Tasks API is Chromium-only" },
          },
        },
      },
    },
  });
  try {
    const perf = new AppStateProvider(client).getChildren().find((n) => n.label === "Performance");
    assert.deepEqual(
      perf?.children?.map((c) => c.label),
      ["DOM Elements"]
    );
  } finally {
    await done();
  }
});

test("IPC Log: a successful command is not marked as failed", async () => {
  const { client, done } = await connected();
  try {
    const { item, icon } = ipcRow(client, "greet");
    assert.equal(icon, "check");
    assert.doesNotMatch(String(item.description), /\[/);
  } finally {
    await done();
  }
});

test("IPC Log: a failed command is marked and its error is in the tooltip", async () => {
  const { client, done } = await connected();
  try {
    const { item, icon } = ipcRow(client, "no_such_command_fixture");
    assert.equal(icon, "error");
    assert.match(String(item.tooltip), /Command no_such_command_fixture not found/);
  } finally {
    await done();
  }
});

test("IPC Log: pending and unknown are neither success nor failure", async () => {
  const entry = (command: string, status: unknown) => ({
    id: 1,
    command,
    status,
    duration_ms: null,
    error: null,
    timestamp: 1,
  });
  const { client, done } = await connected({
    override: {
      "logs/ipc": { body: { result: [entry("a", "pending"), entry("b", "unknown")] } },
    },
  });
  try {
    for (const command of ["a", "b"]) {
      assert.ok(!["check", "error"].includes(ipcRow(client, command).icon), command);
    }
  } finally {
    await done();
  }
});

test("DOM Explorer lists the real snapshot", async () => {
  const { client, done } = await connected();
  try {
    const dom = new DomExplorerProvider(client);
    const top = dom.getChildren();
    assert.ok(top.length > 0);
    assert.match(String(dom.getTreeItem(top[0]).label), /^\[.+\] /);
  } finally {
    await done();
  }
});
