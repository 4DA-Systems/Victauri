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
  constructor(
    public id: string,
    public color?: unknown
  ) {}
}

export class ThemeColor {
  constructor(public id: string) {}
}

export class Range {
  constructor(
    public startLine: number,
    public startChar: number,
    public endLine: number,
    public endChar: number
  ) {}
}

export class CodeLens {
  constructor(
    public range: Range,
    public command?: unknown
  ) {}
}

export enum TreeItemCollapsibleState {
  None = 0,
  Collapsed = 1,
  Expanded = 2,
}

export enum StatusBarAlignment {
  Left = 1,
  Right = 2,
}

export enum ViewColumn {
  Beside = -2,
}

/** Settings the client reads through `workspace.getConfiguration`; tests may override. */
export const stubConfig: Record<string, unknown> = {};
/** The same settings as set by `.vscode/settings.json`: what a hostile workspace controls. */
export const stubWorkspaceConfig: Record<string, unknown> = {};
/** `tauri.conf.json` files `workspace.findFiles` reports, as their file paths. */
export const stubWorkspaceFiles: string[] = [];
/** How many times the extension has searched the workspace. */
export const stubCounters = { findFiles: 0 };

export const workspace = {
  getConfiguration: () => ({
    get: <T>(key: string, fallback: T): T => {
      if (key in stubWorkspaceConfig) return stubWorkspaceConfig[key] as T;
      return key in stubConfig ? (stubConfig[key] as T) : fallback;
    },
    inspect: (key: string) => ({
      key,
      globalValue: stubConfig[key],
      workspaceValue: stubWorkspaceConfig[key],
      workspaceFolderValue: undefined,
    }),
  }),
  findFiles: async () => {
    stubCounters.findFiles++;
    return stubWorkspaceFiles.map((fsPath) => ({ fsPath }));
  },
  openTextDocument: async (doc: { content: string }) => {
    openedDocuments.push(doc.content);
    return doc;
  },
  fs: { writeFile: async () => undefined },
};

/** Every string any toast was asked to show, in order. */
export const shownMessages: Array<{ kind: string; text: string; items: unknown[] }> = [];
/** What a modal `showWarningMessage` answers: tests set the button to "press". */
export const stubAnswers: {
  warning: string | undefined;
  input: string | undefined;
  /** Set to keep a warning open until the promise settles: a dialog the user has not answered. */
  warningOpenUntil?: Promise<void>;
} = {
  warning: undefined,
  input: undefined,
};
export const openedDocuments: string[] = [];
export const statusBar: { tooltip?: unknown; text?: string; command?: unknown } = {};
export const treeProviders: Record<string, { getChildren: (n?: never) => unknown[] }> = {};
export const outputLines: string[] = [];

/** The last webview panel opened, with the HTML it was given. */
export const panels: Array<{
  webview: { html: string; onDidReceiveMessage: (l: (m: unknown) => unknown) => void };
  fireMessage?: (m: unknown) => unknown;
  /** What VS Code does when the user closes the panel. */
  close?: () => void;
}> = [];

const toast =
  (kind: string) =>
  async (text: string, ...items: unknown[]): Promise<string | undefined> => {
    shownMessages.push({ kind, text, items });
    if (kind === "warning") await stubAnswers.warningOpenUntil;
    return kind === "warning" ? stubAnswers.warning : undefined;
  };

export const window = {
  showErrorMessage: toast("error"),
  showWarningMessage: toast("warning"),
  showInformationMessage: toast("info"),
  showInputBox: async () => stubAnswers.input,
  showTextDocument: async () => undefined,
  showSaveDialog: async () => undefined,
  createOutputChannel: () => ({
    appendLine: (l: string) => outputLines.push(l),
    show: () => undefined,
    dispose: () => undefined,
  }),
  createStatusBarItem: () => ({
    set text(v: string) {
      statusBar.text = v;
    },
    set tooltip(v: unknown) {
      statusBar.tooltip = v;
    },
    set command(v: unknown) {
      statusBar.command = v;
    },
    backgroundColor: undefined,
    show: () => undefined,
    dispose: () => undefined,
  }),
  registerTreeDataProvider: (id: string, provider: never) => {
    treeProviders[id] = provider;
    return { dispose: () => undefined };
  },
  createWebviewPanel: () => {
    const panel: (typeof panels)[number] & {
      onDidDispose: (l: () => void) => void;
      reveal: () => void;
    } = {
      webview: {
        html: "",
        onDidReceiveMessage: (l) => {
          panel.fireMessage = l;
        },
      },
      onDidDispose: (l) => {
        panel.close = l;
      },
      reveal: () => undefined,
    };
    panels.push(panel);
    return panel;
  },
};

const handlers = new Map<string, (...args: unknown[]) => unknown>();

export const commands = {
  registerCommand: (id: string, handler: (...args: unknown[]) => unknown) => {
    handlers.set(id, handler);
    return { dispose: () => handlers.delete(id) };
  },
  executeCommand: async (id: string, ...args: unknown[]) => {
    const h = handlers.get(id);
    return h ? h(...args) : undefined;
  },
};

export const languages = {
  registerCodeLensProvider: () => ({ dispose: () => undefined }),
};

export const env = {
  clipboard: { writeText: async () => undefined },
};

export const Uri = {
  file: (fsPath: string) => ({ fsPath }),
};
