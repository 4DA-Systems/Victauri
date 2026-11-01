import * as vscode from "vscode";
import * as fs from "fs/promises";
import { VictauriClient, healthStatusMeansAlive } from "./client";
import { resetOwnerCheckBackoff, resolveConnection, scanServers } from "./discovery";
import { AppStateProvider } from "./appStateView";
import { DomExplorerProvider } from "./domExplorerView";
import { IpcLogProvider } from "./ipcLogView";
import {
  TauriCommandLensProvider,
  generateCommandTest,
} from "./codeLens";
import { ScreenshotPanel } from "./screenshotPanel";
import {
  identifierFromTauriConf,
  readConnectionSettings,
} from "./settings";
import { confirmModal, showError, showInfo, showWarning } from "./toast";

let client: VictauriClient;
let statusBarItem: vscode.StatusBarItem;
let outputChannel: vscode.OutputChannel;

/** How often to look for the app while disconnected with `victauri.autoConnect` on. */
const AUTO_CONNECT_RETRY_MS = 5_000;
const TOKEN_CONSENT_KEY = "victauri.tokenAllowedForPort";

/** `user`: the Connect command. `auto`: activation and the retry timer, which must stay quiet. */
type ConnectMode = "user" | "auto";

let connectQueue: Promise<void> = Promise.resolve();
let connectBusy = false;
/** The user pressed Disconnect: auto-connect leaves them alone until they connect again. */
let autoConnectPaused = false;
/** What auto-connect last said, so a retry every few seconds does not repeat it. */
let lastAutoReport: string | undefined;
let lastAnnouncedApp: string | undefined;

function report(mode: ConnectMode, show: (text: string) => void, text: string): void {
  if (mode === "auto") {
    if (text === lastAutoReport) return;
    lastAutoReport = text;
  }
  show(text);
}

/** Whether anything answers on `port`, asked without a token. */
async function portAnswers(port: number): Promise<boolean> {
  try {
    const resp = await fetch(`http://127.0.0.1:${port}/health`, {
      signal: AbortSignal.timeout(1_500),
    });
    return healthStatusMeansAlive(resp.status);
  } catch {
    return false;
  }
}

/** The identifiers the workspace's Tauri configs name: its own app, among those running. */
async function workspaceIdentifiers(): Promise<string[]> {
  const files = await vscode.workspace.findFiles(
    "**/{tauri.conf.json,tauri.conf.json5,Tauri.toml}",
    "**/{node_modules,target}/**",
    20
  );
  const ids = new Set<string>();
  for (const file of files) {
    try {
      const id = identifierFromTauriConf(file.fsPath, await fs.readFile(file.fsPath, "utf-8"));
      if (id) ids.add(id);
    } catch {
      // unreadable config: it names nothing
    }
  }
  return [...ids];
}

/**
 * The user-level token, if it may go where the settings say. A port from `.vscode/settings.json`
 * is chosen by whoever wrote the repository, so the token goes there only once the user agreed to
 * that port, remembered per workspace (in VS Code's storage, which a repository cannot write).
 * Auto-connect never asks, so a repository cannot raise a dialog just by being opened.
 */
async function tokenForPort(
  context: vscode.ExtensionContext,
  mode: ConnectMode,
  settings: ReturnType<typeof readConnectionSettings>
): Promise<string | undefined> {
  if (!settings.token || !settings.portFromWorkspace) return settings.token;
  if (context.workspaceState.get<number>(TOKEN_CONSENT_KEY) === settings.port) return settings.token;
  if (mode === "auto") return undefined;
  const send = "Send the token";
  const agreed = await confirmModal(
    `This workspace's settings set victauri.port to ${settings.port}. Send your victauri.authToken ` +
      "from user settings to that port? Choose this only if you trust this repository.",
    send
  );
  if (!agreed) return undefined;
  await context.workspaceState.update(TOKEN_CONSENT_KEY, settings.port);
  return settings.token;
}

async function connectOnce(context: vscode.ExtensionContext, mode: ConnectMode): Promise<void> {
  if (mode === "user") resetOwnerCheckBackoff();

  const settings = readConnectionSettings(vscode.workspace.getConfiguration("victauri"));
  if (settings.invalidPort !== undefined) {
    report(
      mode,
      showWarning,
      `Victauri: ignoring victauri.port, which is not a port: ${String(settings.invalidPort).slice(0, 80)}`
    );
  }
  const token = await tokenForPort(context, mode, settings);
  const scan = await scanServers();
  const resolution = resolveConnection(
    { port: settings.port, token },
    scan,
    settings.defaultPort,
    scan.live.length > 0 ? await workspaceIdentifiers() : []
  );
  if (!resolution.ok) {
    report(mode, showError, `Victauri: ${resolution.message}`);
    return;
  }
  // Another project's app is not this workspace's: only a deliberate Connect binds to it.
  if (mode === "auto" && resolution.otherProject) return;
  if (resolution.warning) {
    report(mode, showWarning, `Victauri: ${resolution.warning}`);
  }
  const actualPort = resolution.port;
  // Quiet until there is something to connect to: an app that is not up yet is not an error. And
  // only to an app discovery names or a port the settings name: whatever merely holds the default
  // port may be another app, so binding to that takes a deliberate Connect.
  if (mode === "auto" && !resolution.vouchedBy) {
    if (settings.port === undefined || !(await portAnswers(actualPort))) return;
  }

  try {
    // A discovered endpoint is re-checked against discovery before the token is sent
    // again, and followed by identity if the app restarts (R5-VSC1). An explicitly
    // configured port + token is the user's own choice and is used as given.
    const vouched = resolution.vouchedBy;
    await client.connect(
      actualPort,
      resolution.token,
      vouched
        ? {
            identifier: vouched.identifier,
            discover: async () => (await scanServers()).live,
          }
        : {}
    );
    lastAutoReport = undefined;
    const app = vouched?.identifier;
    if (mode === "user" || app !== lastAnnouncedApp) {
      showInfo(`Victauri: Connected ${app ? `to ${app} ` : ""}on port ${actualPort}`);
    }
    lastAnnouncedApp = app;
    updateStatusBar();
  } catch (e) {
    report(mode, showError, `Victauri: Failed to connect on port ${actualPort} — ${e}`);
  }
}

/** One connect at a time. A timer tick while one is running has nothing to add, so it is dropped. */
function runConnect(context: vscode.ExtensionContext, mode: ConnectMode): Promise<void> {
  if (mode === "auto" && connectBusy) return Promise.resolve();
  if (mode === "user") autoConnectPaused = false;
  connectBusy = true;
  connectQueue = connectQueue
    .then(() => connectOnce(context, mode))
    .finally(() => {
      connectBusy = false;
    });
  return connectQueue;
}

export function activate(context: vscode.ExtensionContext): void {
  client = new VictauriClient();
  autoConnectPaused = false;
  lastAutoReport = undefined;
  lastAnnouncedApp = undefined;
  outputChannel = vscode.window.createOutputChannel("Victauri");

  // Status bar
  statusBarItem = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Left,
    100
  );
  statusBarItem.command = "victauri.connect";
  updateStatusBar();
  statusBarItem.show();

  client.onDidChangeState(() => {
    updateStatusBar();
    vscode.commands.executeCommand(
      "setContext",
      "victauri.connected",
      client.connectionState === "connected"
    );
  });

  // Tree views
  const appStateProvider = new AppStateProvider(client);
  const domProvider = new DomExplorerProvider(client);
  const ipcProvider = new IpcLogProvider(client);

  vscode.window.registerTreeDataProvider("victauri.appState", appStateProvider);
  vscode.window.registerTreeDataProvider("victauri.domExplorer", domProvider);
  vscode.window.registerTreeDataProvider("victauri.ipcLog", ipcProvider);

  // CodeLens
  const codeLens = new TauriCommandLensProvider();
  context.subscriptions.push(
    vscode.languages.registerCodeLensProvider({ language: "rust" }, codeLens)
  );

  // Commands
  context.subscriptions.push(
    vscode.commands.registerCommand("victauri.connect", () => runConnect(context, "user")),

    vscode.commands.registerCommand("victauri.disconnect", () => {
      autoConnectPaused = true;
      client.disconnect();
      showInfo("Victauri: Disconnected");
    }),

    vscode.commands.registerCommand("victauri.refreshAll", () => {
      client.refreshAll().catch((e: unknown) => {
        showError(
          `Victauri: Refresh failed — ${e instanceof Error ? e.message : String(e)}`
        );
      });
    }),

    vscode.commands.registerCommand("victauri.screenshot", async () => {
      if (client.connectionState !== "connected") {
        showWarning("Victauri: Not connected");
        return;
      }
      ScreenshotPanel.show(context, client);
    }),

    vscode.commands.registerCommand("victauri.evalJs", async () => {
      if (client.connectionState !== "connected") {
        showWarning("Victauri: Not connected");
        return;
      }
      const code = await vscode.window.showInputBox({
        prompt: "JavaScript to evaluate in the Tauri webview",
        placeHolder: "document.title",
      });
      if (!code) return;

      try {
        const result = await client.evalJs(code);
        outputChannel.appendLine(`> ${code}`);
        outputChannel.appendLine(JSON.stringify(result, null, 2));
        outputChannel.show();
      } catch (e) {
        showError(`Victauri: Eval failed — ${e}`);
      }
    }),

    vscode.commands.registerCommand("victauri.smokeTest", async () => {
      if (client.connectionState !== "connected") {
        showWarning("Victauri: Not connected");
        return;
      }
      try {
        const result = (await client.callTool("get_diagnostics")) as {
          warnings?: Array<{ message: string }>;
          info?: Record<string, unknown>;
        };
        outputChannel.appendLine("=== Victauri Diagnostics ===");
        outputChannel.appendLine(JSON.stringify(result, null, 2));
        outputChannel.show();

        const warnings = result?.warnings ?? [];
        if (warnings.length === 0) {
          showInfo(
            "Victauri: No compatibility warnings detected"
          );
        } else {
          showWarning(
            `Victauri: ${warnings.length} warning(s) detected — see Output panel`
          );
        }
      } catch (e) {
        showError(`Victauri: Diagnostics failed — ${e}`);
      }
    }),

    vscode.commands.registerCommand("victauri.copyRefId", (node: unknown) => {
      const domNode = node as { ref_id?: string };
      if (domNode?.ref_id) {
        vscode.env.clipboard.writeText(domNode.ref_id);
        showInfo(
          `Copied ref ID: ${domNode.ref_id}`
        );
      }
    }),

    vscode.commands.registerCommand(
      "victauri.generateTest",
      async (node: unknown) => {
        const domNode = node as import("./client").DomNode | undefined;
        if (!domNode?.tag) return;
        const code = domProvider.generateTestCode(domNode);
        const doc = await vscode.workspace.openTextDocument({
          content: code,
          language: "rust",
        });
        await vscode.window.showTextDocument(doc);
      }
    ),

    vscode.commands.registerCommand(
      "victauri.generateTestForCommand",
      async (fnName: unknown) => {
        if (typeof fnName !== "string") return;
        const code = generateCommandTest(fnName);
        const doc = await vscode.workspace.openTextDocument({
          content: code,
          language: "rust",
        });
        await vscode.window.showTextDocument(doc);
      }
    ),

    vscode.commands.registerCommand(
      "victauri.clickElement",
      async (node: unknown) => {
        if (client.connectionState !== "connected") {
          showWarning("Victauri: Not connected");
          return;
        }
        const domNode = node as { ref_id?: string };
        if (!domNode?.ref_id) return;
        try {
          await client.clickElement(domNode.ref_id);
          showInfo(
            `Clicked element ${domNode.ref_id}`
          );
        } catch (e) {
          showError(`Click failed: ${e}`);
        }
      }
    ),

    vscode.commands.registerCommand(
      "victauri.highlightElement",
      async (node: unknown) => {
        if (client.connectionState !== "connected") {
          showWarning("Victauri: Not connected");
          return;
        }
        const domNode = node as { ref_id?: string };
        if (!domNode?.ref_id) return;
        try {
          await client.highlightElement(domNode.ref_id);
        } catch (e) {
          showError(`Highlight failed: ${e}`);
        }
      }
    ),

    vscode.commands.registerCommand("victauri.clearHighlights", async () => {
      if (client.connectionState !== "connected") {
        showWarning("Victauri: Not connected");
        return;
      }
      try {
        await client.clearHighlights();
      } catch (e) {
        showError(`Clear highlights failed: ${e}`);
      }
    }),

    vscode.commands.registerCommand(
      "victauri.inspectStyles",
      async (node: unknown) => {
        if (client.connectionState !== "connected") {
          showWarning("Victauri: Not connected");
          return;
        }
        const domNode = node as { ref_id?: string; tag?: string };
        if (!domNode?.ref_id) return;
        try {
          const styles = await client.getElementStyles(domNode.ref_id);
          outputChannel.appendLine(
            `=== Styles for ${domNode.tag ?? "element"} [${domNode.ref_id}] ===`
          );
          outputChannel.appendLine(JSON.stringify(styles, null, 2));
          outputChannel.show();
        } catch (e) {
          showError(`Inspect styles failed: ${e}`);
        }
      }
    ),

    vscode.commands.registerCommand("victauri.auditA11y", async () => {
      if (client.connectionState !== "connected") {
        showWarning("Victauri: Not connected");
        return;
      }
      try {
        const result = (await client.auditAccessibility()) as {
          violations?: unknown[];
          warnings?: unknown[];
          summary?: Record<string, number>;
        };
        outputChannel.appendLine("=== Accessibility Audit ===");
        outputChannel.appendLine(JSON.stringify(result, null, 2));
        outputChannel.show();

        const violations = result?.violations ?? [];
        const warnings = result?.warnings ?? [];
        if (violations.length === 0 && warnings.length === 0) {
          showInfo(
            "Victauri: No accessibility issues found"
          );
        } else {
          showWarning(
            `Victauri: ${violations.length} violation(s), ${warnings.length} warning(s) — see Output panel`
          );
        }
      } catch (e) {
        showError(`A11y audit failed: ${e}`);
      }
    }),

    vscode.commands.registerCommand("victauri.perfMetrics", async () => {
      if (client.connectionState !== "connected") {
        showWarning("Victauri: Not connected");
        return;
      }
      try {
        const result = await client.getPerformanceMetrics();
        outputChannel.appendLine("=== Performance Metrics ===");
        outputChannel.appendLine(JSON.stringify(result, null, 2));
        outputChannel.show();
      } catch (e) {
        showError(`Performance metrics failed: ${e}`);
      }
    })
  );

  context.subscriptions.push(client, statusBarItem);

  // Auto-connect if configured, and keep looking: the app may start after VS Code, or restart.
  const autoConnectWanted = () =>
    !autoConnectPaused &&
    client.connectionState === "disconnected" &&
    vscode.workspace.getConfiguration("victauri").get<boolean>("autoConnect", true);
  const retry = setInterval(() => {
    if (autoConnectWanted()) void runConnect(context, "auto");
  }, AUTO_CONNECT_RETRY_MS);
  context.subscriptions.push({ dispose: () => clearInterval(retry) });
  if (autoConnectWanted()) void runConnect(context, "auto");
}

export function deactivate(): void {
  client?.dispose();
}

function updateStatusBar(): void {
  switch (client.connectionState) {
    case "connected":
      statusBarItem.text = "$(beaker) Victauri";
      statusBarItem.tooltip = `Connected to ${client.appIdentifier ?? "the app"} on port ${client.connectedPort} — click to disconnect`;
      statusBarItem.command = "victauri.disconnect";
      statusBarItem.backgroundColor = undefined;
      break;
    case "connecting":
      statusBarItem.text = "$(loading~spin) Victauri";
      statusBarItem.tooltip = "Connecting...";
      statusBarItem.command = undefined;
      statusBarItem.backgroundColor = undefined;
      break;
    case "disconnected":
      statusBarItem.text = "$(debug-disconnect) Victauri";
      statusBarItem.tooltip = "Disconnected — click to connect";
      statusBarItem.command = "victauri.connect";
      statusBarItem.backgroundColor = undefined;
      break;
  }
}

