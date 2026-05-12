import * as vscode from "vscode";
import { VictauriClient } from "./client";

type NodeKind =
  | "header"
  | "window"
  | "memory"
  | "plugin"
  | "diagnosticWarning";

interface StateNode {
  kind: NodeKind;
  label: string;
  description?: string;
  tooltip?: string;
  children?: StateNode[];
  icon?: vscode.ThemeIcon;
}

export class AppStateProvider implements vscode.TreeDataProvider<StateNode> {
  private readonly changeEmitter = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this.changeEmitter.event;

  constructor(private readonly client: VictauriClient) {
    client.onDidUpdateData(() => this.changeEmitter.fire());
    client.onDidChangeState(() => this.changeEmitter.fire());
  }

  getTreeItem(node: StateNode): vscode.TreeItem {
    const item = new vscode.TreeItem(
      node.label,
      node.children?.length
        ? vscode.TreeItemCollapsibleState.Expanded
        : vscode.TreeItemCollapsibleState.None
    );
    item.description = node.description;
    item.tooltip = node.tooltip;
    item.iconPath = node.icon;
    item.contextValue = node.kind;
    return item;
  }

  getChildren(node?: StateNode): StateNode[] {
    if (node) return node.children ?? [];

    if (this.client.connectionState !== "connected") {
      return [
        {
          kind: "header",
          label: "Not connected",
          description: "Run 'Victauri: Connect' to start",
          icon: new vscode.ThemeIcon("debug-disconnect"),
        },
      ];
    }

    const nodes: StateNode[] = [];

    // Windows
    const windows = this.client.windows;
    if (windows.length > 0) {
      nodes.push({
        kind: "header",
        label: "Windows",
        icon: new vscode.ThemeIcon("window"),
        children: windows.map((w) => ({
          kind: "window" as NodeKind,
          label: w.label,
          description: `${w.size[0]}x${w.size[1]}${w.visible ? "" : " (hidden)"}`,
          tooltip: `${w.title}\n${w.url}\n${w.size[0]}x${w.size[1]} at (${w.position[0]}, ${w.position[1]})`,
          icon: new vscode.ThemeIcon(
            w.visible ? "browser" : "eye-closed"
          ),
        })),
      });
    }

    // Memory
    const mem = this.client.memoryStats;
    const workingSet = mem.working_set_bytes as number | undefined;
    if (workingSet) {
      const mb = (workingSet / 1_048_576).toFixed(1);
      nodes.push({
        kind: "memory",
        label: "Memory",
        description: `${mb} MB`,
        icon: new vscode.ThemeIcon("dashboard"),
      });
    }

    // Plugin info
    const info = this.client.pluginInfo;
    if (info.version) {
      nodes.push({
        kind: "plugin",
        label: "Plugin",
        description: `v${info.version}`,
        icon: new vscode.ThemeIcon("extensions"),
        children: [
          {
            kind: "plugin",
            label: "Tools",
            description: `${this.client.toolCount} available`,
          },
          {
            kind: "plugin",
            label: "Uptime",
            description: `${Math.round(info.uptime_secs as number)}s`,
          },
          {
            kind: "plugin",
            label: "Invocations",
            description: `${info.tool_invocations}`,
          },
        ],
      });
    }

    // Diagnostics warnings
    const diag = this.client.diagnostics;
    if (diag && diag.warnings.length > 0) {
      nodes.push({
        kind: "header",
        label: "Warnings",
        icon: new vscode.ThemeIcon("warning"),
        children: diag.warnings.map((w) => ({
          kind: "diagnosticWarning" as NodeKind,
          label: w.id,
          description: w.severity,
          tooltip: w.message,
          icon: new vscode.ThemeIcon("warning"),
        })),
      });
    }

    return nodes;
  }
}
