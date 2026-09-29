// Discovery of a running Victauri app: its port and Bearer token, from the per-process
// directories the plugin writes. Mirrors `victauri-test::discovery` (the reference
// implementation); kept free of `vscode` imports so it can be exercised under plain Node.
import * as os from "os";
import * as path from "path";
import * as fs from "fs/promises";

export interface DiscoveredServer {
  port: number;
  token: string | undefined;
  /** Bundle identifier from `metadata.json`, when the plugin recorded one. */
  identifier?: string;
  pid?: number;
}

// Rust's `std::env::temp_dir()`: `TMPDIR` (else `/tmp`) on Unix; `TMP`, then `TEMP` on Windows.
function tempDir(): string {
  if (process.platform === "win32") {
    return process.env.TMP || process.env.TEMP || os.tmpdir();
  }
  return process.env.TMPDIR || "/tmp";
}

// Where discovery directories live, most specific first. On Unix the root is per-user —
// `$XDG_RUNTIME_DIR/victauri` when that directory is private to us, else
// `<temp>/victauri-<euid>` — so another local user cannot pre-create the shared root and
// block discovery; the legacy shared `<temp>/victauri` (0.8.x apps) is read last. Windows
// `%TEMP%` is already per-user.
export async function discoveryRoots(): Promise<string[]> {
  const tmp = tempDir();
  const legacy = path.join(tmp, "victauri");
  if (process.platform === "win32") return [legacy];
  const roots: string[] = [];
  const euid = typeof process.geteuid === "function" ? process.geteuid() : -1;
  if (euid >= 0) {
    const runtime = process.env.XDG_RUNTIME_DIR;
    if (runtime && path.isAbsolute(runtime)) {
      try {
        const st = await fs.lstat(runtime);
        if (st.isDirectory() && st.uid === euid && (st.mode & 0o077) === 0) {
          roots.push(path.join(runtime, "victauri"));
        }
      } catch {
        // no usable runtime dir
      }
    }
    roots.push(path.join(tmp, `victauri-${euid}`));
  }
  roots.push(legacy);
  return roots;
}

// Whether a discovery dir is safe to trust (audit #9): on Unix it must be a real directory
// (not a symlink), owned by the current user, and not group/other-writable. Windows temp is
// per-user and the writer restricts the directory's ACL, so no extra check is needed there.
export async function dirIsTrusted(dir: string): Promise<boolean> {
  if (process.platform === "win32") return true;
  try {
    const st = await fs.lstat(dir);
    if (!st.isDirectory()) return false;
    const euid = typeof process.geteuid === "function" ? process.geteuid() : -1;
    if (euid >= 0 && st.uid !== euid) return false;
    if ((st.mode & 0o022) !== 0) return false;
    return true;
  } catch {
    return false;
  }
}

/**
 * What `process.kill(pid, 0)` says about a discovery entry's owner:
 * - `own` — a live process we may signal: ours. Its token may be used.
 * - `other` — (Unix) EPERM: it exists but belongs to another user — a recycled PID. Never
 *   ours; never use its token (the cross-user PID-reuse class, audit R2-7).
 * - `unverified` — (Windows) EPERM: it exists but we may not open it — plausibly our own app
 *   running elevated while VS Code is not. Not trusted for a token, but reported, so the user
 *   learns why nothing was found (R4-DISC1).
 * - `dead` — ESRCH: no such process.
 */
export type Liveness = "own" | "other" | "unverified" | "dead";

export function pidLiveness(pid: number): Liveness {
  if (!Number.isInteger(pid) || pid <= 0) return "dead";
  try {
    process.kill(pid, 0);
    return "own";
  } catch (e) {
    const code = (e as NodeJS.ErrnoException | undefined)?.code;
    if (code === "ESRCH") return "dead";
    if (code === "EPERM") return process.platform === "win32" ? "unverified" : "other";
    return "unverified";
  }
}

// Whether `pid` is a live process OWNED BY US (see `pidLiveness`).
export function pidIsAlive(pid: number): boolean {
  return pidLiveness(pid) === "own";
}

async function readServer(dir: string, pid: number): Promise<DiscoveredServer | undefined> {
  try {
    const portStr = (await fs.readFile(path.join(dir, "port"), "utf-8")).trim();
    if (!/^\d+$/.test(portStr)) return undefined;
    const port = parseInt(portStr, 10);
    if (!Number.isInteger(port) || port <= 0 || port >= 65536) return undefined;
    let token: string | undefined;
    try {
      const t = (await fs.readFile(path.join(dir, "token"), "utf-8")).trim();
      if (t) token = t;
    } catch {
      // no token file
    }
    let identifier: string | undefined;
    try {
      const meta = JSON.parse(await fs.readFile(path.join(dir, "metadata.json"), "utf-8"));
      if (typeof meta?.identifier === "string" && meta.identifier) identifier = meta.identifier;
    } catch {
      // no / unreadable metadata
    }
    return { port, token, identifier, pid };
  } catch {
    return undefined; // no port file in this dir
  }
}

export interface DiscoveryScan {
  /** Live, trusted entries owned by us. */
  live: DiscoveredServer[];
  /** PIDs of entries whose process exists but could not be verified as ours. */
  unverified: number[];
}

// Every live, trusted app across all roots (a PID found in two roots counts once). Read-only:
// nothing is ever deleted.
export async function scanServers(
  liveness: (pid: number) => Liveness = pidLiveness,
  roots?: string[]
): Promise<DiscoveryScan> {
  const byPid = new Map<string, DiscoveredServer>();
  const unverified: number[] = [];
  for (const root of roots ?? (await discoveryRoots())) {
    // The root owner can swap a previously checked child directory. Refuse a root's
    // whole tree unless the root itself is trusted.
    if (!(await dirIsTrusted(root))) continue;
    let entries;
    try {
      entries = await fs.readdir(root, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      if (!entry.isDirectory() || !/^\d+$/.test(entry.name) || byPid.has(entry.name)) continue;
      const dir = path.join(root, entry.name);
      // Only trust a discovery dir we own — never read a token from a dir a local
      // attacker could have planted (audit #9).
      if (!(await dirIsTrusted(dir))) continue;
      const pid = parseInt(entry.name, 10);
      const state = liveness(pid);
      if (state === "unverified") {
        if (!unverified.includes(pid)) unverified.push(pid);
        continue;
      }
      // Skip dirs left behind by an exited app (a crash / kill leaves them on disk) and
      // PIDs recycled by another user: one stale dir next to the live one would make
      // discovery ambiguous.
      if (state !== "own") continue;
      const server = await readServer(dir, pid);
      if (server) byPid.set(entry.name, server);
    }
  }
  return { live: [...byPid.values()], unverified };
}

export type Resolution =
  | {
      ok: true;
      port: number;
      token: string | undefined;
      warning?: string;
      /**
       * The live discovery entry whose token is used, when discovery chose (or matched) it.
       * The client then keeps re-checking discovery before sending that token (R5-VSC1).
       * Absent for an explicitly configured port + token, or when no token is sent.
       */
      vouchedBy?: DiscoveredServer;
    }
  | { ok: false; message: string };

function label(s: DiscoveredServer): string {
  const pid = s.pid !== undefined ? `, pid ${s.pid}` : "";
  return `${s.identifier ?? "<unknown app>"} (port ${s.port}${pid})`;
}

/**
 * Pick the endpoint to connect to. Mirrors `victauri-test`'s `try_resolve_connection`:
 * - an explicitly configured port is used, with the configured token or else the token of
 *   the ONE live entry on that port;
 * - a configured token WITHOUT a configured port is sent only to the live app whose own
 *   discovery token equals it — never to whatever holds the default port (R4-TOK1);
 * - otherwise the single live app; several are an error naming each (never a silent pick);
 *   none falls back to the default port with no token.
 */
export function resolveConnection(
  explicit: { port?: number; token?: string },
  scan: DiscoveryScan,
  defaultPort: number
): Resolution {
  const servers = scan.live;
  if (explicit.port !== undefined) {
    const onPort = servers.filter((s) => s.port === explicit.port);
    if (explicit.token) return { ok: true, port: explicit.port, token: explicit.token };
    if (onPort.length === 1 && onPort[0].token) {
      return { ok: true, port: explicit.port, token: onPort[0].token, vouchedBy: onPort[0] };
    }
    return { ok: true, port: explicit.port, token: undefined };
  }
  if (explicit.token) {
    const owners = servers.filter((s) => s.token === explicit.token);
    if (owners.length === 1) {
      return { ok: true, port: owners[0].port, token: explicit.token, vouchedBy: owners[0] };
    }
    if (owners.length === 0) {
      return {
        ok: false,
        message:
          "victauri.authToken is set, but no running Victauri app's discovery entry carries " +
          "that token, so it was not sent anywhere (without victauri.port it would have gone " +
          "to whatever process holds the default port). Set victauri.port to the app's port " +
          "to use this token explicitly, or clear victauri.authToken to use the discovered one.",
      };
    }
    return {
      ok: false,
      message: `Several Victauri apps carry that token: ${owners.map(label).join(", ")}. Set victauri.port to choose one.`,
    };
  }
  if (servers.length === 1) {
    const only = servers[0];
    return only.token
      ? { ok: true, port: only.port, token: only.token, vouchedBy: only }
      : { ok: true, port: only.port, token: undefined };
  }
  if (servers.length > 1) {
    return {
      ok: false,
      message: `Multiple Victauri apps are running: ${servers.map(label).join(", ")}. Set victauri.port to the one to connect to.`,
    };
  }
  const warning =
    scan.unverified.length > 0
      ? `A Victauri app appears to be running (pid ${scan.unverified.join(", ")}) but its ` +
        "process could not be verified as yours — typically it runs elevated while VS Code " +
        "does not. Run both with the same privileges, or set victauri.port and victauri.authToken."
      : undefined;
  return { ok: true, port: defaultPort, token: undefined, warning };
}

// The single live, trusted app, or the default port with NO token when there is none or
// more than one. (Kept for callers that only need the old best-effort answer.)
export async function discoverServer(defaultPort: number): Promise<DiscoveredServer> {
  const { live } = await scanServers();
  if (live.length === 1) return live[0];
  return { port: defaultPort, token: undefined };
}
