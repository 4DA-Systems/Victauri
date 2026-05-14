import * as vscode from "vscode";
import { DomNode, VictauriClient } from "./client";

export class DomExplorerProvider
  implements vscode.TreeDataProvider<DomNode>
{
  private readonly changeEmitter = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this.changeEmitter.event;

  constructor(private readonly client: VictauriClient) {
    client.onDidUpdateData(() => this.changeEmitter.fire());
    client.onDidChangeState(() => this.changeEmitter.fire());
  }

  getTreeItem(node: DomNode): vscode.TreeItem {
    const hasChildren = node.children && node.children.length > 0;
    const item = new vscode.TreeItem(
      this.formatLabel(node),
      hasChildren
        ? vscode.TreeItemCollapsibleState.Collapsed
        : vscode.TreeItemCollapsibleState.None
    );

    item.description = this.formatDescription(node);
    item.tooltip = this.formatTooltip(node);
    item.contextValue = "domElement";
    item.iconPath = new vscode.ThemeIcon(this.iconForTag(node.tag));

    return item;
  }

  getChildren(node?: DomNode): DomNode[] {
    if (this.client.connectionState !== "connected") return [];

    if (!node) {
      const root = this.client.domSnapshot;
      return root?.children ?? (root ? [root] : []);
    }
    return node.children ?? [];
  }

  getRefId(node: DomNode): string | undefined {
    return node.ref_id;
  }

  generateTestCode(node: DomNode): string {
    const lines: string[] = [];
    lines.push('e2e_test!(test_element, |client| async move {');

    if (node.ref_id) {
      if (node.tag === "input" || node.tag === "textarea") {
        lines.push(
          `    client.fill("${node.ref_id}", "test value").await.unwrap();`
        );
      } else if (node.tag === "button" || node.tag === "a") {
        lines.push(
          `    client.click("${node.ref_id}").await.unwrap();`
        );
      } else if (node.name) {
        lines.push(
          `    Locator::text("${node.name}")`,
          `        .expect(&mut client)`,
          `        .to_be_visible()`,
          `        .await`,
          `        .unwrap();`
        );
      }
    }

    lines.push("});");
    return lines.join("\n");
  }

  private formatLabel(node: DomNode): string {
    const ref = node.ref_id ? `[${node.ref_id}]` : "";
    return `${ref} ${node.tag}`.trim();
  }

  private formatDescription(node: DomNode): string {
    const parts: string[] = [];
    if (node.role && node.role !== "generic") parts.push(node.role);
    if (node.name) parts.push(`"${this.truncate(node.name, 30)}"`);
    return parts.join(" ");
  }

  private formatTooltip(node: DomNode): string {
    const lines = [`<${node.tag}>`];
    if (node.ref_id) lines.push(`ref: ${node.ref_id}`);
    if (node.role) lines.push(`role: ${node.role}`);
    if (node.name) lines.push(`name: ${node.name}`);
    if (node.text) lines.push(`text: ${this.truncate(node.text, 100)}`);
    if (node.bounds) {
      lines.push(
        `bounds: ${node.bounds.x},${node.bounds.y} ${node.bounds.width}x${node.bounds.height}`
      );
    }
    return lines.join("\n");
  }

  private iconForTag(tag: string): string {
    switch (tag) {
      case "button":
        return "debug-start";
      case "input":
      case "textarea":
        return "text-size";
      case "a":
        return "link";
      case "img":
        return "file-media";
      case "div":
      case "span":
        return "symbol-misc";
      case "h1":
      case "h2":
      case "h3":
      case "h4":
      case "h5":
      case "h6":
        return "symbol-text";
      case "ul":
      case "ol":
        return "list-unordered";
      case "table":
        return "table";
      case "form":
        return "checklist";
      case "nav":
        return "compass";
      case "main":
      case "section":
      case "article":
        return "layout";
      default:
        return "symbol-misc";
    }
  }

  private truncate(s: string, maxLen: number): string {
    return s.length > maxLen ? s.slice(0, maxLen) + "..." : s;
  }
}
