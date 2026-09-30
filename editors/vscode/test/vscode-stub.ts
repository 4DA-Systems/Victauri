// Minimal stand-in for the `vscode` module so extension code can be unit-tested under plain
// Node (bundled with `--alias:vscode=./test/vscode-stub.ts`). Only what module scope and
// the tested code paths touch.

export class EventEmitter<T> {
  private listeners: Array<(e: T) => void> = [];
  event = (listener: (e: T) => void) => {
    this.listeners.push(listener);
    return { dispose: () => undefined };
  };
  fire(e: T): void {
    for (const l of this.listeners) l(e);
  }
  dispose(): void {
    this.listeners = [];
  }
}

export class TreeItem {
  description?: string;
  tooltip?: string;
  contextValue?: string;
  iconPath?: unknown;
  constructor(
    public label: string,
    public collapsibleState?: number
  ) {}
}

export class ThemeIcon {
  constructor(public id: string) {}
}

export enum TreeItemCollapsibleState {
  None = 0,
  Collapsed = 1,
  Expanded = 2,
}

/** Settings the client reads through `workspace.getConfiguration`; tests may override. */
export const stubConfig: Record<string, unknown> = {};

export const workspace = {
  getConfiguration: () => ({
    get: <T>(key: string, fallback: T): T => (key in stubConfig ? (stubConfig[key] as T) : fallback),
  }),
};

export const window = {
  showWarningMessage: (..._args: unknown[]) => undefined,
};
