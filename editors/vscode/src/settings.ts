// Reading the extension's settings. `.vscode/settings.json` can set them, and VS Code does not
// enforce the schema at runtime, so a value is never trusted to be the type it claims. Kept free
// of `vscode` imports so it can be tested under plain Node.

const DEFAULT_PORT = 7373;
const DEFAULT_POLL_MS = 2000;
const MIN_POLL_MS = 500;
// Above 2^31 - 1 ms Node's timers fire after 1 ms instead.
const MAX_POLL_MS = 3_600_000;

export function validPort(value: unknown): number | undefined {
  return typeof value === "number" && Number.isInteger(value) && value > 0 && value < 65536
    ? value
    : undefined;
}

/** A usable polling interval from whatever the setting holds: at most one poll per 500 ms. */
export function pollIntervalMs(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return DEFAULT_POLL_MS;
  return Math.min(MAX_POLL_MS, Math.max(MIN_POLL_MS, value));
}

interface Inspected {
  globalValue?: unknown;
  workspaceValue?: unknown;
  workspaceFolderValue?: unknown;
}

/** The part of `WorkspaceConfiguration` used here. */
export interface SettingsSource {
  inspect(key: string): Inspected | undefined;
}

export interface ConnectionSettings {
  /** The configured port (vs. its default); a configured token may only go to this. */
  port: number | undefined;
  /** The port to try when nothing else names one. */
  defaultPort: number;
  /** `victauri.port` holds something that is not a port (ignored): for a warning. */
  invalidPort: unknown;
  /** The port came from workspace settings, which whoever wrote the repository controls. */
  portFromWorkspace: boolean;
  token: string | undefined;
}

export function readConnectionSettings(config: SettingsSource): ConnectionSettings {
  const p = config.inspect("port");
  const scoped = p?.workspaceFolderValue ?? p?.workspaceValue;
  const raw = scoped ?? p?.globalValue;
  const port = validPort(raw);
  // The token is user-level only (`"scope": "machine"` in the manifest). Reading the global value
  // says so here too: a repository must not get to supply it, nor to redirect it unasked.
  const rawToken = config.inspect("authToken")?.globalValue;
  const token = typeof rawToken === "string" && rawToken.trim() ? rawToken.trim() : undefined;
  return {
    port,
    defaultPort: DEFAULT_PORT,
    invalidPort: raw !== undefined && port === undefined ? raw : undefined,
    portFromWorkspace: port !== undefined && scoped !== undefined,
    token,
  };
}

/** The app identifier a `tauri.conf.json` / `.json5` / `Tauri.toml` names, if it can be read. */
export function identifierFromTauriConf(fileName: string, text: string): string | undefined {
  if (/\.json$/i.test(fileName)) {
    try {
      const conf = JSON.parse(text) as {
        identifier?: unknown;
        tauri?: { bundle?: { identifier?: unknown } };
      };
      const id = conf?.identifier ?? conf?.tauri?.bundle?.identifier; // Tauri 2, then Tauri 1
      return typeof id === "string" && id ? id : undefined;
    } catch {
      return undefined;
    }
  }
  return /^\s*"?identifier"?\s*[:=]\s*["']([^"']+)["']/m.exec(text)?.[1];
}
