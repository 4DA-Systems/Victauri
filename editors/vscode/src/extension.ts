import * as vscode from "vscode";
import * as path from "path";
import * as fs from "fs";
import { VictauriClient } from "./client";
import { AppStateProvider } from "./appStateView";
import { DomExplorerProvider } from "./domExplorerView";
import { IpcLogProvider } from "./ipcLogView";
import {
  TauriCommandLensProvider,
  generateCommandTest,
} from "./codeLens";
import { ScreenshotPanel } from "./screenshotPanel";

let client: VictauriClient;
let statusBarItem: vscode.StatusBarItem;
let outputChannel: vscode.OutputChannel;

export function activate(context: vscode.ExtensionContext): void {
  client = new VictauriClient();
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
    vscode.commands.registerCommand("victauri.connect", async () => {
      const config = vscode.workspace.getConfiguration("victauri");
      const port = config.get<number>("port", 7373);
      const token = config.get<string>("authToken", "");

      // Try to auto-discover port from victauri.port file
      const actualPort = await discoverPort(port);

      try {
        await client.connect(actualPort, token || undefined);
        vscode.window.showInformationMessage(
          `Victauri: Connected on port ${actualPort}`
        );
      } catch (e) {
        vscode.window.showErrorMessage(
          `Victauri: Failed to connect on port ${actualPort} — ${e}`
        );
      }
    }),

    vscode.commands.registerCommand("victauri.disconnect", () => {
      client.disconnect();
      vscode.window.showInformationMessage("Victauri: Disconnected");
    }),

    vscode.commands.registerCommand("victauri.refreshAll", () => {
      client.refreshAll();
    }),

    vscode.commands.registerCommand("victauri.screenshot", async () => {
      if (client.connectionState !== "connected") {
        vscode.window.showWarningMessage("Victauri: Not connected");
        return;
      }
      ScreenshotPanel.show(context, client);
    }),

    vscode.commands.registerCommand("victauri.evalJs", async () => {
      if (client.connectionState !== "connected") {
        vscode.window.showWarningMessage("Victauri: Not connected");
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
        vscode.window.showErrorMessage(`Victauri: Eval failed — ${e}`);
      }
    }),

    vscode.commands.registerCommand("victauri.smokeTest", async () => {
      if (client.connectionState !== "connected") {
        vscode.window.showWarningMessage("Victauri: Not connected");
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
          vscode.window.showInformationMessage(
            "Victauri: No compatibility warnings detected"
          );
        } else {
          vscode.window.showWarningMessage(
            `Victauri: ${warnings.length} warning(s) detected — see Output panel`
          );
        }
      } catch (e) {
        vscode.window.showErrorMessage(`Victauri: Diagnostics failed — ${e}`);
      }
    }),

    vscode.commands.registerCommand("victauri.copyRefId", (node: unknown) => {
      const domNode = node as { ref_id?: string };
      if (domNode?.ref_id) {
        vscode.env.clipboard.writeText(domNode.ref_id);
        vscode.window.showInformationMessage(
          `Copied ref ID: ${domNode.ref_id}`
        );
      }
    }),

    vscode.commands.registerCommand(
      "victauri.generateTest",
      async (node: unknown) => {
        const domNode = node as {
          ref_id?: string;
          tag?: string;
          name?: string;
        };
        const code = domProvider.generateTestCode(
          domNode as import("./client").DomNode
        );
        const doc = await vscode.workspace.openTextDocument({
          content: code,
          language: "rust",
        });
        await vscode.window.showTextDocument(doc);
      }
    ),

    vscode.commands.registerCommand(
      "victauri.generateTestForCommand",
      async (fnName: string) => {
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
        const domNode = node as { ref_id?: string };
        if (!domNode?.ref_id) return;
        try {
          await client.clickElement(domNode.ref_id);
          vscode.window.showInformationMessage(
            `Clicked element ${domNode.ref_id}`
          );
        } catch (e) {
          vscode.window.showErrorMessage(`Click failed: ${e}`);
        }
      }
    ),

    vscode.commands.registerCommand(
      "victauri.highlightElement",
      async (node: unknown) => {
        const domNode = node as { ref_id?: string };
        if (!domNode?.ref_id) return;
        try {
          await client.highlightElement(domNode.ref_id);
        } catch (e) {
          vscode.window.showErrorMessage(`Highlight failed: ${e}`);
        }
      }
    ),

    vscode.commands.registerCommand("victauri.clearHighlights", async () => {
      try {
        await client.clearHighlights();
      } catch (e) {
        vscode.window.showErrorMessage(`Clear highlights failed: ${e}`);
      }
    }),

    vscode.commands.registerCommand(
      "victauri.inspectStyles",
      async (node: unknown) => {
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
          vscode.window.showErrorMessage(`Inspect styles failed: ${e}`);
        }
      }
    ),

    vscode.commands.registerCommand("victauri.auditA11y", async () => {
      if (client.connectionState !== "connected") {
        vscode.window.showWarningMessage("Victauri: Not connected");
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
          vscode.window.showInformationMessage(
            "Victauri: No accessibility issues found"
          );
        } else {
          vscode.window.showWarningMessage(
            `Victauri: ${violations.length} violation(s), ${warnings.length} warning(s) — see Output panel`
          );
        }
      } catch (e) {
        vscode.window.showErrorMessage(`A11y audit failed: ${e}`);
      }
    }),

    vscode.commands.registerCommand("victauri.perfMetrics", async () => {
      if (client.connectionState !== "connected") {
        vscode.window.showWarningMessage("Victauri: Not connected");
        return;
      }
      try {
        const result = await client.getPerformanceMetrics();
        outputChannel.appendLine("=== Performance Metrics ===");
        outputChannel.appendLine(JSON.stringify(result, null, 2));
        outputChannel.show();
      } catch (e) {
        vscode.window.showErrorMessage(`Performance metrics failed: ${e}`);
      }
    })
  );

  context.subscriptions.push(client, statusBarItem);

  // Auto-connect if configured
  const autoConnect = vscode.workspace
    .getConfiguration("victauri")
    .get<boolean>("autoConnect", true);
  if (autoConnect) {
    vscode.commands.executeCommand("victauri.connect");
  }
}

export function deactivate(): void {
  client?.dispose();
}

function updateStatusBar(): void {
  switch (client.connectionState) {
    case "connected":
      statusBarItem.text = "$(beaker) Victauri";
      statusBarItem.tooltip = "Connected — click to disconnect";
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

async function discoverPort(defaultPort: number): Promise<number> {
  // Check temp dir for victauri.port file
  const tmpDir =
    process.env.TMPDIR ?? process.env.TEMP ?? process.env.TMP ?? "/tmp";
  const portFile = path.join(tmpDir, "victauri.port");
  try {
    const content = fs.readFileSync(portFile, "utf-8").trim();
    const port = parseInt(content, 10);
    if (port > 0 && port < 65536) return port;
  } catch {
    // File doesn't exist or unreadable
  }
  return defaultPort;
}
