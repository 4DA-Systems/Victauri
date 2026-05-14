import * as vscode from "vscode";

export interface ToolInfo {
  name: string;
  description?: string;
}

export interface WindowState {
  label: string;
  title: string;
  url: string;
  visible: boolean;
  focused: boolean;
  size: [number, number];
  position: [number, number];
}

export interface IpcEntry {
  command: string;
  timestamp: number;
  status: number;
  duration_ms: number;
  method: string;
  url: string;
}

export interface DomNode {
  tag: string;
  ref_id?: string;
  role?: string;
  name?: string;
  text?: string;
  visible?: boolean;
  children?: DomNode[];
  bounds?: { x: number; y: number; width: number; height: number };
}

export interface DiagnosticsResult {
  warnings: Array<{
    id: string;
    severity: string;
    message: string;
    details?: Record<string, unknown>;
  }>;
  info: Record<string, unknown>;
}

export type ConnectionState = "disconnected" | "connecting" | "connected";

export class VictauriClient {
  private baseUrl = "";
  private token = "";
  private state: ConnectionState = "disconnected";
  private pollTimer: ReturnType<typeof setInterval> | undefined;
  private readonly onStateChange = new vscode.EventEmitter<ConnectionState>();
  private readonly onDataUpdate = new vscode.EventEmitter<void>();

  readonly onDidChangeState = this.onStateChange.event;
  readonly onDidUpdateData = this.onDataUpdate.event;

  // Cached data from last poll
  windows: WindowState[] = [];
  ipcLog: IpcEntry[] = [];
  domSnapshot: DomNode | null = null;
  memoryStats: Record<string, unknown> = {};
  pluginInfo: Record<string, unknown> = {};
  diagnostics: DiagnosticsResult | null = null;
  perfMetrics: Record<string, unknown> | null = null;
  toolCount = 0;

  get connectionState(): ConnectionState {
    return this.state;
  }

  async connect(port: number, authToken?: string): Promise<void> {
    this.baseUrl = `http://127.0.0.1:${port}`;
    this.token = authToken ?? "";
    this.setState("connecting");

    try {
      const resp = await this.fetch("/health");
      if (!resp.ok) {
        throw new Error(`Health check failed: ${resp.status}`);
      }
      this.setState("connected");
      await this.refreshAll();
      this.startPolling();
    } catch (e) {
      this.setState("disconnected");
      throw e;
    }
  }

  disconnect(): void {
    this.stopPolling();
    this.setState("disconnected");
    this.windows = [];
    this.ipcLog = [];
    this.domSnapshot = null;
    this.memoryStats = {};
    this.pluginInfo = {};
    this.diagnostics = null;
    this.perfMetrics = null;
    this.toolCount = 0;
  }

  async refreshAll(): Promise<void> {
    if (this.state !== "connected") return;
    await Promise.allSettled([
      this.refreshWindows(),
      this.refreshIpcLog(),
      this.refreshMemory(),
      this.refreshPluginInfo(),
      this.refreshDom(),
      this.refreshDiagnostics(),
      this.refreshPerformance(),
    ]);
    this.onDataUpdate.fire();
  }

  async callTool(
    name: string,
    args: Record<string, unknown> = {}
  ): Promise<unknown> {
    const resp = await this.fetch(`/api/tools/${name}`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(args),
    });
    const body = (await resp.json()) as {
      result?: unknown;
      error?: string;
    };
    if (!resp.ok || body.error) {
      throw new Error(body.error ?? `HTTP ${resp.status}`);
    }
    return body.result;
  }

  async screenshot(): Promise<string | null> {
    const result = (await this.callTool("screenshot")) as {
      type?: string;
      data?: string;
    } | null;
    if (result && typeof result === "object" && "data" in result) {
      return result.data as string;
    }
    return null;
  }

  async evalJs(code: string): Promise<unknown> {
    return this.callTool("eval_js", { code });
  }

  async clickElement(refId: string): Promise<unknown> {
    return this.callTool("interact", { action: "click", ref_id: refId });
  }

  async highlightElement(refId: string): Promise<unknown> {
    return this.callTool("inspect", { action: "highlight", ref_id: refId, color: "#e94560" });
  }

  async clearHighlights(): Promise<unknown> {
    return this.callTool("inspect", { action: "highlight_clear" });
  }

  async getElementStyles(refId: string): Promise<unknown> {
    return this.callTool("inspect", { action: "styles", ref_id: refId });
  }

  async auditAccessibility(): Promise<unknown> {
    return this.callTool("inspect", { action: "accessibility" });
  }

  async getPerformanceMetrics(): Promise<unknown> {
    return this.callTool("inspect", { action: "performance" });
  }

  dispose(): void {
    this.disconnect();
    this.onStateChange.dispose();
    this.onDataUpdate.dispose();
  }

  private async refreshWindows(): Promise<void> {
    try {
      const result = (await this.callTool("window", {
        action: "list",
      })) as string[];
      if (Array.isArray(result)) {
        const states: WindowState[] = [];
        for (const label of result) {
          try {
            const s = (await this.callTool("window", {
              action: "get_state",
              webview_label: label,
            })) as WindowState;
            states.push(s);
          } catch {
            // skip windows that fail
          }
        }
        this.windows = states;
      }
    } catch {
      // keep stale data
    }
  }

  private async refreshIpcLog(): Promise<void> {
    try {
      const result = (await this.callTool("logs", {
        source: "ipc",
        limit: 50,
      })) as IpcEntry[];
      if (Array.isArray(result)) {
        this.ipcLog = result;
      }
    } catch {
      // keep stale data
    }
  }

  private async refreshMemory(): Promise<void> {
    try {
      const result = (await this.callTool(
        "get_memory_stats"
      )) as Record<string, unknown>;
      if (result && typeof result === "object") {
        this.memoryStats = result;
      }
    } catch {
      // keep stale
    }
  }

  private async refreshPluginInfo(): Promise<void> {
    try {
      const result = (await this.callTool(
        "get_plugin_info"
      )) as Record<string, unknown>;
      if (result && typeof result === "object") {
        this.pluginInfo = result;
        const tools = result.tools as { total?: number } | undefined;
        this.toolCount = tools?.total ?? 0;
      }
    } catch {
      // keep stale
    }
  }

  private async refreshDiagnostics(): Promise<void> {
    try {
      const result = (await this.callTool(
        "get_diagnostics"
      )) as DiagnosticsResult;
      if (result && typeof result === "object" && "warnings" in result) {
        this.diagnostics = result;
      }
    } catch {
      // keep stale
    }
  }

  private async refreshPerformance(): Promise<void> {
    try {
      const result = (await this.getPerformanceMetrics()) as Record<string, unknown>;
      if (result && typeof result === "object") {
        this.perfMetrics = result;
      }
    } catch {
      // keep stale
    }
  }

  private async refreshDom(): Promise<void> {
    try {
      const result = (await this.callTool("dom_snapshot", {
        format: "json",
      })) as { body?: DomNode } | null;
      if (result && typeof result === "object" && "body" in result) {
        this.domSnapshot = result.body as DomNode;
      }
    } catch {
      // keep stale
    }
  }

  private startPolling(): void {
    this.stopPolling();
    const interval = vscode.workspace
      .getConfiguration("victauri")
      .get<number>("pollInterval", 2000);
    this.pollTimer = setInterval(() => {
      this.refreshAll().catch(() => {
        // if server went down, disconnect
        this.disconnect();
        vscode.window.showWarningMessage(
          "Victauri: Lost connection to Tauri app"
        );
      });
    }, interval);
  }

  private stopPolling(): void {
    if (this.pollTimer) {
      clearInterval(this.pollTimer);
      this.pollTimer = undefined;
    }
  }

  private setState(s: ConnectionState): void {
    this.state = s;
    this.onStateChange.fire(s);
  }

  private async fetch(
    path: string,
    init?: RequestInit
  ): Promise<Response> {
    const headers: Record<string, string> = {
      ...(init?.headers as Record<string, string>),
    };
    if (this.token) {
      headers["Authorization"] = `Bearer ${this.token}`;
    }
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 30_000);
    try {
      return await fetch(`${this.baseUrl}${path}`, {
        ...init,
        headers,
        signal: controller.signal,
      });
    } finally {
      clearTimeout(timeout);
    }
  }
}
