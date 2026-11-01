import * as vscode from "vscode";
import { IpcEntry, VictauriClient } from "./client";

/**
 * What an entry's status means. The bridge reports `ok` / `error` / `pending` / `unknown`; the
 * HTTP status says nothing, as a failed Tauri command answers 200 too. Numbers are only for a
 * plugin older than that field.
 */
function outcome(status: unknown): "ok" | "error" | "neutral" {
  if (status === "ok" || status === 200) return "ok";
  if (status === "pending" || status === "unknown") return "neutral";
  return "error";
}

export class IpcLogProvider
  implements vscode.TreeDataProvider<IpcEntry>
{
  private readonly changeEmitter = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this.changeEmitter.event;

  constructor(private readonly client: VictauriClient) {
    client.onDidUpdateData(() => this.changeEmitter.fire());
    client.onDidChangeState(() => this.changeEmitter.fire());
  }

  getTreeItem(entry: IpcEntry): vscode.TreeItem {
    const item = new vscode.TreeItem(
      entry.command,
      vscode.TreeItemCollapsibleState.None
    );

    const duration = entry.duration_ms != null ? `${entry.duration_ms}ms` : "";
    const result = outcome(entry.status);
    const status = result === "ok" ? "" : ` [${entry.status}]`;
    item.description = `${duration}${status}`;

    item.tooltip = [
      `Command: ${entry.command}`,
      `Status: ${entry.status}`,
      `Duration: ${entry.duration_ms ?? "?"}ms`,
      `Time: ${new Date(entry.timestamp).toLocaleTimeString()}`,
      ...(entry.error ? [`Error: ${entry.error}`] : []),
    ].join("\n");

    item.iconPath =
      result === "ok"
        ? new vscode.ThemeIcon("check", new vscode.ThemeColor("testing.iconPassed"))
        : result === "error"
          ? new vscode.ThemeIcon("error", new vscode.ThemeColor("testing.iconFailed"))
          : new vscode.ThemeIcon("circle-outline");

    item.contextValue = "ipcEntry";
    return item;
  }

  getChildren(element?: IpcEntry): IpcEntry[] {
    if (element) return [];
    if (this.client.connectionState !== "connected") return [];
    return [...this.client.ipcLog].reverse();
  }
}
