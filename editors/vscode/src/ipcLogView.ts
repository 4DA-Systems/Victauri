import * as vscode from "vscode";
import { IpcEntry, VictauriClient } from "./client";

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
    const status = entry.status === 200 ? "" : ` [${entry.status}]`;
    item.description = `${duration}${status}`;

    item.tooltip = [
      `Command: ${entry.command}`,
      `Status: ${entry.status}`,
      `Duration: ${entry.duration_ms ?? "?"}ms`,
      `Time: ${new Date(entry.timestamp).toLocaleTimeString()}`,
    ].join("\n");

    item.iconPath = new vscode.ThemeIcon(
      entry.status === 200 ? "check" : "error",
      entry.status === 200
        ? new vscode.ThemeColor("testing.iconPassed")
        : new vscode.ThemeColor("testing.iconFailed")
    );

    item.contextValue = "ipcEntry";
    return item;
  }

  getChildren(element?: IpcEntry): IpcEntry[] {
    if (element) return [];
    if (this.client.connectionState !== "connected") return [];
    return [...this.client.ipcLog].reverse();
  }
}
