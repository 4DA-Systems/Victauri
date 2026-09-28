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

/**
 * Whether a `GET /health` status proves the server is alive: a 2xx, or a 429 — `/health` is
 * unauthenticated and rate-limited from the public bucket, so any local process can flood it
 * into 429s, and a rate-limited reply is still the server answering (R4-NET1).
 */
export function healthStatusMeansAlive(status: number): boolean {
  return (status >= 200 && status < 300) || status === 429;
}

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
      if (!healthStatusMeansAlive(resp.status)) {
        throw new Error(`Health check failed: ${resp.status}`);
      }
      // `/health` is deliberately unauthenticated, so a green health check says
      // nothing about whether our token works. Probe an auth-gated endpoint too,
      // so a wrong/missing token fails here instead of every refresh 401-ing
      // silently afterwards.
      await this.probeAuthenticated();
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

  /**
   * Authenticated liveness probe (`GET /info` sits behind the auth layer).
   * Throws on a network error, a 401, or any non-2xx status.
   */
  async probeAuthenticated(): Promise<void> {
    const resp = await this.fetch("/info");
    // Rate-limited: the server answered, so it is alive — don't report a disconnect because
    // a local process is flooding the public rate-limit bucket (R4-NET1).
    if (resp.status === 429) return;
    if (resp.status === 401) {
      throw new Error(
        "Unauthorized (401): the auth token is missing or wrong. Auth is on by " +
          "default — the token is in the app's discovery directory (<pid>/token), or set " +
          "`victauri.authToken`."
      );
    }
    if (!resp.ok) {
      throw new Error(`Authenticated probe (/info) failed: HTTP ${resp.status}`);
    }
  }

  async refreshAll(): Promise<void> {
    if (this.state !== "connected") return;
    // Each per-view refresh below swallows its own errors (keeping stale data
    // for a flaky tool), so they can't signal a dead server. Probe first and
    // let that failure propagate so the poller detects the disconnection.
    await this.probeAuthenticated();
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
    const text = await resp.text();
    let body: { result?: unknown; error?: string } | undefined;
    try {
      body = JSON.parse(text) as { result?: unknown; error?: string };
    } catch {
      body = undefined;
    }
    if (resp.status === 401) {
      throw new Error("Unauthorized (401): auth token missing or wrong");
    }
    if (!resp.ok || body === undefined || body.error) {
      throw new Error(
        body?.error ??
          `HTTP ${resp.status}: ${text.slice(0, 200) || resp.statusText}`
      );
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
    return this.callTool("inspect", { action: "clear_highlights" });
  }

  async getElementStyles(refId: string): Promise<unknown> {
    return this.callTool("inspect", { action: "get_styles", ref_id: refId });
  }

  async auditAccessibility(): Promise<unknown> {
    return this.callTool("inspect", { action: "audit_accessibility" });
  }

  async getPerformanceMetrics(): Promise<unknown> {
    return this.callTool("inspect", { action: "get_performance" });
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
              label,
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
        action: "ipc",
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
      // dom_snapshot returns `{ tree, stale_refs, format }`. The default
      // "compact" format makes `tree` an indented text string; the explorer
      // needs the structured element tree, so request `format: "json"`, where
      // `tree` is the root DomNode (document.body) — or null if body is hidden.
      const result = (await this.callTool("dom_snapshot", {
        format: "json",
      })) as { tree?: DomNode | string | null } | null;
      if (result && typeof result === "object" && "tree" in result) {
        const tree = result.tree;
        this.domSnapshot =
          tree && typeof tree === "object" ? (tree as DomNode) : null;
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
      this.refreshAll().catch((e: unknown) => {
        // Server went down (or the token stopped working, e.g. the app was
        // restarted and minted a fresh one): disconnect so the UI says so.
        if (this.state !== "connected") return;
        this.disconnect();
        this.onDataUpdate.fire();
        vscode.window.showWarningMessage(
          `Victauri: Lost connection to Tauri app — ${e instanceof Error ? e.message : String(e)}`
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
