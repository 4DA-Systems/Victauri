import * as vscode from "vscode";

import type { DiscoveredServer } from "./discovery";
import { pollIntervalMs } from "./settings";
import { showWarning } from "./toast";

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
  /** `ok` / `error` / `pending` / `unknown` (the bridge's reading of Tauri's outcome). */
  status: string | number;
  duration_ms: number | null;
  /** The command's error text, when `status` is `error`. */
  error?: string | null;
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
 * Limits on one request, including its body. An app may legitimately return a large screenshot
 * (a 4K PNG is tens of MB in base64), so the size limit is generous. Tests shorten them.
 */
export const requestLimits = { timeoutMs: 30_000, maxResponseBytes: 64 * 1024 * 1024 };

/** A response whose body has already been read, within `requestLimits`. */
interface BufferedResponse {
  status: number;
  ok: boolean;
  statusText: string;
  headers: Headers;
  text: string;
}

function isWindowState(w: unknown): w is WindowState {
  if (typeof w !== "object" || w === null) return false;
  const { label, size, position } = w as Partial<WindowState>;
  const pair = (p: unknown) =>
    Array.isArray(p) && p.length === 2 && p.every((n) => typeof n === "number");
  return typeof label === "string" && pair(size) && pair(position);
}

const BASE64 = /^[A-Za-z0-9+/]+={0,2}$/;

/**
 * Whether a `GET /health` status proves the server is alive: a 2xx, or a 429 — `/health` is
 * unauthenticated and rate-limited from the public bucket, so any local process can flood it
 * into 429s, and a rate-limited reply is still the server answering (R4-NET1).
 */
export function healthStatusMeansAlive(status: number): boolean {
  return (status >= 200 && status < 300) || status === 429;
}

/** What an `/info` answer says about the auth probe. */
export type AuthProbeVerdict = "ok" | "unauthorized" | "rate-limited" | "error";

/**
 * Classify the authenticated `/info` probe's status. A `429` is NOT a pass: a request carrying
 * the correct token draws from its own rate-limit bucket, which an unauthenticated flood cannot
 * drain, so a 429 here most likely means the token was not accepted (round-4 review) — the
 * caller retries and then reports it instead of showing "connected".
 */
export function authProbeVerdict(status: number): AuthProbeVerdict {
  if (status === 401) return "unauthorized";
  if (status === 429) return "rate-limited";
  return status >= 200 && status < 300 ? "ok" : "error";
}

/**
 * How the client keeps a discovered endpoint honest (R5-VSC1). With `discover`, every request
 * that carries the token first re-reads discovery and requires a live, trusted entry that still
 * maps this port to this token (and `identifier`). If the app restarted elsewhere, the client
 * follows it by `identifier` and verifies `/info`'s `app_identifier` before any other request
 * carries the new token; if nothing vouches for the port, the token is not sent.
 */
export interface ConnectOptions {
  /** Bundle identifier of the discovered app (from its discovery `metadata.json`). */
  identifier?: string;
  /** The live, trusted discovery entries right now (`scanServers().live`). */
  discover?: () => Promise<DiscoveredServer[]>;
}

export class VictauriClient {
  private baseUrl = "";
  private port = 0;
  private token = "";
  private identifier: string | undefined;
  private discover: (() => Promise<DiscoveredServer[]>) | undefined;
  /** Set when an endpoint proved to be a different app: nothing is sent until reconnect. */
  private refusedReason: string | undefined;
  private state: ConnectionState = "disconnected";
  private pollTimer: ReturnType<typeof setInterval> | undefined;
  /** A poll is running: the next tick is skipped rather than overlapping it (R5-VSC1). */
  private refreshing = false;
  /** Which `startPolling` is current: a poll left over from an earlier one is ignored. */
  private pollGeneration = 0;
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

  /** The discovered app's identifier, while connected to one. */
  get appIdentifier(): string | undefined {
    return this.identifier;
  }

  /** The port currently connected to (it changes when a restarted app is followed). */
  get connectedPort(): number {
    return this.port;
  }

  async connect(port: number, authToken?: string, options: ConnectOptions = {}): Promise<void> {
    this.pointAt(port, authToken ?? "");
    this.identifier = options.identifier;
    this.discover = options.discover;
    this.refusedReason = undefined;
    this.setState("connecting");

    try {
      const resp = await this.rawFetch("/health", undefined, false);
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
      // The user may have pressed Disconnect during that first load.
      if (this.state === "connected") this.startPolling();
    } catch (e) {
      this.setState("disconnected");
      throw e;
    }
  }

  disconnect(): void {
    this.stopPolling();
    // Nothing is sent from here until the next connect: an open screenshot panel still has a
    // Refresh button.
    this.pointAt(0, "");
    this.identifier = undefined;
    this.discover = undefined;
    this.refusedReason = undefined;
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
    let resp = await this.fetch("/info");
    // A 429 is transient only for a caller whose token is accepted (its own bucket); retry a
    // few times, honouring Retry-After, and then say so rather than claim "connected".
    for (let attempt = 0; attempt < 3 && authProbeVerdict(resp.status) === "rate-limited"; attempt++) {
      const wait = Math.min(5, Number(resp.headers.get("retry-after")) || 1);
      await new Promise((r) => setTimeout(r, wait * 1000));
      resp = await this.fetch("/info");
    }
    if (authProbeVerdict(resp.status) === "rate-limited") {
      throw new Error(
        "Rate-limited (429) on an authenticated request: the auth token was probably not " +
          "accepted (a correct token has its own rate-limit bucket). Check `victauri.authToken` " +
          "or the app's discovery token."
      );
    }
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
    await this.checkIdentity(resp);
  }

  /**
   * The `/info` answer must name the app we discovered: a port can change hands, and a
   * restarted app is re-resolved by identity (R5-VSC1). On a mismatch nothing more is sent to
   * this endpoint until the user reconnects.
   */
  private async checkIdentity(resp: BufferedResponse): Promise<void> {
    if (this.identifier === undefined) return;
    let reported: unknown;
    try {
      reported = (JSON.parse(resp.text) as { app_identifier?: unknown } | null)?.app_identifier;
    } catch {
      reported = undefined;
    }
    if (reported !== this.identifier) {
      this.refusedReason =
        `port ${this.port} answers as ${typeof reported === "string" ? reported : "an unknown app"}, ` +
        `not ${this.identifier}; nothing more is sent to it. Reconnect once the app is running.`;
      throw new Error(`Victauri: ${this.refusedReason}`);
    }
  }

  /**
   * Before the token goes out: confirm through discovery that this port still belongs to the
   * app we connected to. Discovery (a trusted, own-process entry carrying this exact port and
   * token) is the only proof available before sending the token: `/info` itself requires it.
   */
  private async ensureEndpointTrusted(): Promise<void> {
    if (this.refusedReason) throw new Error(`Victauri: ${this.refusedReason}`);
    if (!this.discover || !this.token) return;
    const live = await this.discover();
    const sameApp = (s: DiscoveredServer) =>
      this.identifier === undefined || s.identifier === this.identifier;
    if (live.some((s) => s.port === this.port && s.token === this.token && sameApp(s))) return;

    // Not vouched for any more. Follow the SAME app (by identity) if it restarted elsewhere.
    const moved =
      this.identifier === undefined
        ? []
        : live.filter((s) => s.identifier === this.identifier && s.token);
    if (moved.length !== 1) {
      throw new Error(
        `Victauri: port ${this.port} no longer belongs to ` +
          `${this.identifier ?? "the app this window connected to"} (its discovery entry is ` +
          "gone), so the auth token was not sent. Reconnect once the app is running."
      );
    }
    this.pointAt(moved[0].port, moved[0].token ?? "");
    // Verify the new endpoint's identity before any other request carries the token.
    await this.checkIdentity(await this.rawFetch("/info"));
  }

  private pointAt(port: number, token: string): void {
    this.port = port;
    this.baseUrl = `http://127.0.0.1:${port}`;
    this.token = token;
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
    if (this.state !== "connected") throw new Error("Victauri: not connected");
    const resp = await this.fetch(`/api/tools/${name}`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(args),
    });
    const text = resp.text;
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
    // Anything else would land in the panel's HTML: this answer can come from whatever holds
    // the port, not only from the plugin.
    const data = result && typeof result === "object" ? result.data : undefined;
    return typeof data === "string" && BASE64.test(data) ? data : null;
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
      // With no label this answers with every window, as an array.
      const result = await this.callTool("window", { action: "get_state" });
      if (Array.isArray(result)) {
        this.windows = result.filter(isWindowState);
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
    const interval = pollIntervalMs(
      vscode.workspace.getConfiguration("victauri").get<unknown>("pollInterval")
    );
    this.refreshing = false;
    const generation = ++this.pollGeneration;
    this.pollTimer = setInterval(() => {
      // A refresh on a slow app can outlast the interval: skip this tick instead of stacking
      // a second concurrent refresh on top of it (R5-VSC1).
      if (this.refreshing) return;
      this.refreshing = true;
      this.pollOnce()
        .catch((e: unknown) => {
          // Server went down (or the token stopped working, e.g. the app was
          // restarted and minted a fresh one): disconnect so the UI says so.
          if (generation !== this.pollGeneration || this.state !== "connected") return;
          this.disconnect();
          this.onDataUpdate.fire();
          showWarning(
            `Victauri: Lost connection to Tauri app — ${e instanceof Error ? e.message : String(e)}`
          );
        })
        .finally(() => {
          if (generation === this.pollGeneration) this.refreshing = false;
        });
    }, interval);
  }

  /**
   * One poll. On a CONNECTION error (the app exited or is restarting) retry once: that retry
   * re-resolves through discovery before sending anything, following the same app to its new
   * port or refusing to send the token at all.
   */
  private async pollOnce(): Promise<void> {
    try {
      await this.refreshAll();
    } catch (e) {
      if (!isConnectionError(e) || this.state !== "connected") throw e;
      await this.refreshAll();
    }
  }

  private stopPolling(): void {
    this.pollGeneration++;
    if (this.pollTimer) {
      clearInterval(this.pollTimer);
      this.pollTimer = undefined;
    }
  }

  private setState(s: ConnectionState): void {
    this.state = s;
    this.onStateChange.fire(s);
  }

  /** Every request but `/health` goes through here; a token-bearing one is vouched for first. */
  private async fetch(path: string, init?: RequestInit): Promise<BufferedResponse> {
    await this.ensureEndpointTrusted();
    return this.rawFetch(path, init);
  }

  /** The timeout and size limit cover the body too: a stalled body must not hang the poller. */
  private async rawFetch(
    path: string,
    init?: RequestInit,
    sendToken = true
  ): Promise<BufferedResponse> {
    const headers: Record<string, string> = {
      ...(init?.headers as Record<string, string>),
    };
    if (this.token && sendToken) {
      headers["Authorization"] = `Bearer ${this.token}`;
    }
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), requestLimits.timeoutMs);
    try {
      const resp = await fetch(`${this.baseUrl}${path}`, {
        ...init,
        headers,
        // A 307 would resend a tool call's body to wherever the answer points.
        redirect: "error",
        signal: controller.signal,
      });
      return {
        status: resp.status,
        ok: resp.ok,
        statusText: resp.statusText,
        headers: resp.headers,
        text: await readCapped(resp, requestLimits.maxResponseBytes),
      };
    } catch (e) {
      if (!controller.signal.aborted) throw e;
      throw new Error(
        `Victauri: port ${this.port} did not answer ${path} within ${requestLimits.timeoutMs} ms`
      );
    } finally {
      clearTimeout(timeout);
    }
  }
}

async function readCapped(resp: Response, limit: number): Promise<string> {
  if (!resp.body) return "";
  const reader = resp.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.length;
    if (total > limit) {
      await reader.cancel().catch(() => undefined);
      throw new Error(`Victauri: refused an answer larger than ${limit} bytes`);
    }
    chunks.push(value);
  }
  return Buffer.concat(chunks).toString("utf-8");
}

/** `fetch` rejects with a `TypeError` when no HTTP response arrived at all. */
function isConnectionError(e: unknown): boolean {
  return e instanceof TypeError;
}
