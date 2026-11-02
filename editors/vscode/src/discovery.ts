// Discovery of a running Victauri app: its port and Bearer token, from the per-process
// directories the plugin writes. Mirrors `victauri-test::discovery` (the reference
// implementation); kept free of `vscode` imports so it can be exercised under plain Node.
import * as os from "os";
import * as path from "path";
import * as fs from "fs/promises";
import { execFile } from "child_process";

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

// The plugin's per-user fallback discovery root under the home directory (Unix):
// `$XDG_STATE_HOME/victauri`, else `$HOME/.local/state/victauri` (each only when absolute). The
// plugin registers there when `<temp>/victauri-<euid>` is untrusted — another local user can
// pre-create that predictable path, and sticky `/tmp` stops us deleting it (R5B-LINDISC1).
export function homeStateRoot(env: NodeJS.ProcessEnv = process.env): string | undefined {
  const state = env.XDG_STATE_HOME;
  if (state && path.posix.isAbsolute(state)) return path.posix.join(state, "victauri");
  const home = env.HOME;
  if (home && path.posix.isAbsolute(home)) {
    return path.posix.join(home, ".local", "state", "victauri");
  }
  return undefined;
}

// Where discovery directories live, most specific first — the order every Victauri reader
// scans. On Unix the roots are per-user: `$XDG_RUNTIME_DIR/victauri` when that directory is
// private to us, then `<temp>/victauri-<euid>` (so another local user cannot pre-create a
// shared root and block discovery), then the home fallback above; the legacy shared
// `<temp>/victauri` (0.8.x apps) is read last. Windows `%TEMP%` is normally per-user (and
// ownership is verified, see below).
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
    const home = homeStateRoot();
    if (home) roots.push(home);
  }
  roots.push(legacy);
  return roots;
}

// ── Windows: discovery-directory ownership (R5B-WINDISC1) ────────────────────────────────
//
// `%TEMP%` is normally per-user, but it can be shared — an app launched from MSYS2 uses
// `C:\msys64\tmp` — and there another user can plant `victauri\<live pid>\{port,token,…}`
// pointing at a port they control. The Rust readers compare the directory's owner SID with the
// process token via Win32; the extension cannot call Win32 directly, so:
//
// - a directory whose REAL path is inside the user's profile (`os.homedir()`, which is where the
//   default `%LOCALAPPDATA%\Temp` lives) is trusted without a check: Windows grants only SYSTEM,
//   Administrators and the user access to a profile, so no other non-admin user can plant
//   anything there — the check would cost a PowerShell start for nothing. The real path, because
//   a junction relocating TEMP looks like a profile path while leading anywhere;
// - anywhere else, the owner is read with PowerShell's `Get-Acl` (one call for a batch of
//   directories, 30 s timeout — a cold Windows PowerShell start has been measured above 10 s on
//   a CI runner) and must be the current user, the token's default owner, or
//   `BUILTIN\Administrators` when this token is a member — the plugin writer's own rule. A
//   definite verdict is cached per directory identity (inode + birth time), so a recreated
//   directory is checked again. It fails CLOSED: if PowerShell cannot run, times out or answers
//   garbage, such a directory is not trusted for that scan — and that failure is NOT cached, so
//   a later scan asks again instead of distrusting the app's directory for as long as it lives
//   (set `victauri.port` + `victauri.authToken` explicitly where PowerShell cannot run). Later,
//   not the next: every token-bearing request scans, so concurrent requests share one query and
//   a failure backs off, or a broken PowerShell costs a process start per request.

export interface OwnerCheckOptions {
  /** The profile directory treated as private (default `os.homedir()`); tests override it. */
  profileDir?: string;
  /** Replaces the PowerShell query; tests count and fail it. */
  queryOwners?: (dirs: string[]) => Promise<boolean[] | null>;
  /** Set when an owner check could not give an answer, so directories went untrusted for it. */
  report?: { ownerCheckFailed: boolean };
}

const ownerVerdicts = new Map<string, { id: string; trusted: boolean }>();
const OWNER_RETRY_BACKOFF_MS = 30_000;
const ownerQueriesInFlight = new Map<string, Promise<boolean[] | null>>();
/** Directories whose last owner check gave no answer, and when to ask again. */
const ownerRetryAt = new Map<string, number>();

/** Forget the backoff: the user just asked to connect, so a fresh attempt is worth its cost. */
export function resetOwnerCheckBackoff(): void {
  ownerRetryAt.clear();
}

async function realPathOf(p: string): Promise<string | undefined> {
  try {
    return await fs.realpath(p);
  } catch {
    return undefined;
  }
}

function isInside(child: string, parent: string): boolean {
  const rel = path.relative(parent.toLowerCase(), child.toLowerCase());
  return rel === "" || (!!rel && !rel.startsWith("..") && !path.isAbsolute(rel));
}

const OWNER_SCRIPT = [
  "$ErrorActionPreference = 'Stop'",
  "$id = [Security.Principal.WindowsIdentity]::GetCurrent()",
  "$ok = @($id.User.Value, $id.Owner.Value)",
  // Group SIDs and deny-only SIDs are both claims; an unelevated admin holds 544 deny-only.
  "if ($id.Claims | Where-Object { $_.Value -eq 'S-1-5-32-544' }) { $ok += 'S-1-5-32-544' }",
  // No @(...) here: Windows PowerShell 5.1 emits a JSON array as ONE object, which @() would
  // wrap instead of enumerate.
  "$paths = ConvertFrom-Json $env:VICTAURI_OWNER_PATHS",
  "$out = @(foreach ($p in $paths) { try { $ok -contains (Get-Acl -LiteralPath $p).GetOwner([Security.Principal.SecurityIdentifier]).Value } catch { $false } })",
  "ConvertTo-Json -Compress -InputObject $out",
].join("; ");

/**
 * The environment for the Windows PowerShell 5.1 owner check. `PSModulePath` is dropped: a
 * VS Code started from a PowerShell 7 terminal (`code .`) passes on pwsh's module path, and
 * Windows PowerShell then loads PowerShell 7's incompatible `Microsoft.PowerShell.Security` —
 * `Get-Acl` fails, and every directory outside the profile read as not owned (fail-closed, so
 * the app was never found). Without the variable, 5.1 uses its own default module path.
 */
export function ownerCheckEnv(
  base: NodeJS.ProcessEnv,
  dirs: string[]
): Record<string, string | undefined> {
  const env: Record<string, string | undefined> = {};
  for (const [k, v] of Object.entries(base)) {
    if (k.toLowerCase() !== "psmodulepath") env[k] = v;
  }
  env.VICTAURI_OWNER_PATHS = JSON.stringify(dirs);
  return env;
}

/**
 * Ask PowerShell whether each of `dirs` is owned by the current user. `null` when there is no
 * definite answer — PowerShell failed to run or timed out, or its output did not parse or did not
 * answer every directory: the caller then trusts none of them for now and does not cache it.
 */
function queryOwners(dirs: string[]): Promise<boolean[] | null> {
  const systemRoot = process.env.SystemRoot || "C:\\Windows";
  const ps = path.join(systemRoot, "System32", "WindowsPowerShell", "v1.0", "powershell.exe");
  return new Promise((resolve) => {
    execFile(
      ps,
      ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", OWNER_SCRIPT],
      {
        env: ownerCheckEnv(process.env, dirs),
        timeout: 30_000,
        windowsHide: true,
      },
      (err, stdout) => {
        if (err) return resolve(null);
        try {
          const parsed: unknown = JSON.parse(String(stdout).trim());
          const arr = Array.isArray(parsed) ? parsed : [parsed];
          if (arr.length !== dirs.length || arr.some((v) => typeof v !== "boolean")) {
            return resolve(null);
          }
          resolve(arr as boolean[]);
        } catch {
          resolve(null);
        }
      }
    );
  });
}

/** One query at a time for one set of directories, however many requests are scanning. */
function sharedOwnerQuery(
  dirs: string[],
  query: (dirs: string[]) => Promise<boolean[] | null>
): Promise<boolean[] | null> {
  const key = JSON.stringify(dirs);
  const running = ownerQueriesInFlight.get(key);
  if (running) return running;
  const started = query(dirs).finally(() => ownerQueriesInFlight.delete(key));
  ownerQueriesInFlight.set(key, started);
  return started;
}

/** Windows: which of `dirs` (already known to be real directories) are owned by us. */
async function windowsOwnedDirs(
  dirs: string[],
  opts: OwnerCheckOptions = {}
): Promise<Set<string>> {
  const profile = await realPathOf(opts.profileDir ?? os.homedir());
  const trusted = new Set<string>();
  const pending: { dir: string; id: string }[] = [];
  for (const dir of dirs) {
    const real = await realPathOf(dir);
    if (profile && real && isInside(real, profile)) {
      trusted.add(dir);
      continue;
    }
    let id: string;
    try {
      const st = await fs.lstat(dir, { bigint: true });
      id = `${st.ino}:${st.birthtimeNs}`;
    } catch {
      continue;
    }
    const cached = ownerVerdicts.get(dir);
    if (cached && cached.id === id) {
      if (cached.trusted) trusted.add(dir);
      continue;
    }
    if ((ownerRetryAt.get(dir) ?? 0) > Date.now()) {
      if (opts.report) opts.report.ownerCheckFailed = true;
      continue;
    }
    pending.push({ dir, id });
  }
  if (pending.length > 0) {
    const verdicts = await sharedOwnerQuery(
      pending.map((p) => p.dir),
      opts.queryOwners ?? queryOwners
    );
    // No definite answer: trust none of them now (fail closed), but cache nothing — one slow
    // or failed PowerShell start must not distrust the app's directory for as long as it lives.
    if (verdicts !== null) {
      pending.forEach(({ dir, id }, i) => {
        ownerRetryAt.delete(dir);
        ownerVerdicts.set(dir, { id, trusted: verdicts[i] });
        if (verdicts[i]) trusted.add(dir);
      });
    } else {
      if (opts.report) opts.report.ownerCheckFailed = true;
      const retryAt = Date.now() + OWNER_RETRY_BACKOFF_MS;
      for (const { dir } of pending) ownerRetryAt.set(dir, retryAt);
    }
  }
  return trusted;
}

// Whether a discovery dir is safe to trust (audit #9): it must be a real directory (not a
// symlink or junction) owned by the current user. On Unix: owned by our euid and not
// group/other-writable. On Windows: owned by us (see the ownership section above; R5B-WINDISC1).
export async function dirIsTrusted(dir: string, opts: OwnerCheckOptions = {}): Promise<boolean> {
  return (await trustedDirs([dir], opts)).has(dir);
}

// The subset of `dirs` that are trusted discovery directories — batched, so on Windows one
// PowerShell call (at most) checks the owners of every uncached directory at once.
async function trustedDirs(dirs: string[], opts: OwnerCheckOptions = {}): Promise<Set<string>> {
  const real: string[] = [];
  for (const dir of dirs) {
    try {
      const st = await fs.lstat(dir);
      if (!st.isDirectory()) continue;
      if (process.platform !== "win32") {
        const euid = typeof process.geteuid === "function" ? process.geteuid() : -1;
        if (euid >= 0 && st.uid !== euid) continue;
        if ((st.mode & 0o022) !== 0) continue;
      }
      real.push(dir);
    } catch {
      // missing / unreadable
    }
  }
  if (process.platform !== "win32") return new Set(real);
  return windowsOwnedDirs(real, opts);
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

/** What the server on a port said to one entry's token. */
type InfoAnswer = { kind: "accepted"; pid?: number } | { kind: "refused" } | { kind: "unknown" };

async function askInfo(port: number, token: string | undefined): Promise<InfoAnswer> {
  try {
    const resp = await fetch(`http://127.0.0.1:${port}/info`, {
      headers: token ? { Authorization: `Bearer ${token}` } : {},
      signal: AbortSignal.timeout(800),
    });
    if (resp.status === 401) return { kind: "refused" };
    if (resp.status !== 200) return { kind: "unknown" };
    const info = (await resp.json()) as { pid?: unknown } | null;
    const pid = info?.pid;
    return { kind: "accepted", pid: typeof pid === "number" ? pid : undefined };
  } catch {
    return { kind: "unknown" };
  }
}

// Drop the entries that are not the server on their port, when several claim one: a hard-killed
// app's entry stays behind, and once its PID is reused it looks as live as the current app's,
// naming the same port. Mirrors `victauri-test`'s `real_port_claims`: the pid `/info` reports
// decides; an older plugin reports none, and then accepting the token does; entries nothing could
// be learned about stay. One claim per port, the normal case, sends nothing.
async function settlePortClaims(live: DiscoveredServer[]): Promise<DiscoveredServer[]> {
  const ports = new Set(live.map((s) => s.port));
  const dropped = new Set<DiscoveredServer>();
  for (const port of ports) {
    const claims = live.filter((s) => s.port === port);
    if (claims.length < 2) continue;
    const answers = await Promise.all(claims.map((s) => askInfo(port, s.token)));
    const reported = answers.find((a) => a.kind === "accepted" && a.pid !== undefined);
    const anyAccepted = answers.some((a) => a.kind === "accepted");
    claims.forEach((claim, i) => {
      const keep =
        reported?.kind === "accepted"
          ? claim.pid === reported.pid
          : anyAccepted
            ? answers[i].kind === "accepted"
            : answers[i].kind === "unknown";
      if (!keep) dropped.add(claim);
    });
  }
  return live.filter((s) => !dropped.has(s));
}

export interface DiscoveryScan {
  /** Live, trusted entries owned by us. */
  live: DiscoveredServer[];
  /** PIDs of entries whose process exists but could not be verified as ours. */
  unverified: number[];
  /** A directory went untrusted only because its owner could not be checked. */
  ownerCheckFailed?: boolean;
}

// Every live, trusted app across all roots (a PID found in two roots counts once). Read-only:
// nothing is ever deleted.
export async function scanServers(
  liveness: (pid: number) => Liveness = pidLiveness,
  roots?: string[],
  opts: OwnerCheckOptions = {}
): Promise<DiscoveryScan> {
  const byPid = new Map<string, DiscoveredServer>();
  const unverified: number[] = [];
  const report = { ownerCheckFailed: false };
  opts = { ...opts, report };
  for (const root of roots ?? (await discoveryRoots())) {
    // The root owner can swap a previously checked child directory. Refuse a root's
    // whole tree unless the root itself is trusted.
    if (!(await dirIsTrusted(root, opts))) continue;
    let entries;
    try {
      entries = await fs.readdir(root, { withFileTypes: true });
    } catch {
      continue;
    }
    const candidates = entries
      .filter((e) => e.isDirectory() && /^\d+$/.test(e.name) && !byPid.has(e.name))
      .map((e) => path.join(root, e.name));
    // Only trust a discovery dir we own — never read a token from a dir a local
    // attacker could have planted (audit #9, R5B-WINDISC1). One batched check per root.
    const trusted = await trustedDirs(candidates, opts);
    for (const dir of candidates) {
      const name = path.basename(dir);
      if (!trusted.has(dir) || byPid.has(name)) continue;
      const pid = parseInt(name, 10);
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
      if (server) byPid.set(name, server);
    }
  }
  return {
    live: await settlePortClaims([...byPid.values()]),
    unverified,
    ...(report.ownerCheckFailed ? { ownerCheckFailed: true } : {}),
  };
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
      /**
       * The workspace names its app and this is not it: the only app running belongs to some
       * other project. A manual connect warns; auto-connect stays away from it.
       */
      otherProject?: boolean;
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
 * - otherwise the workspace's own app (`workspaceIdentifiers`, from its `tauri.conf.json`), else
 *   the single live app; several are an error naming each (never a silent pick); none falls back
 *   to the default port with no token, unless discovery could not be read at all: a 401 from
 *   whatever holds the port would hide the real cause.
 */
export function resolveConnection(
  explicit: { port?: number; token?: string },
  scan: DiscoveryScan,
  defaultPort: number,
  workspaceIdentifiers: string[] = []
): Resolution {
  const servers = scan.live;
  const ownerCheckWarning = scan.ownerCheckFailed
    ? "The owner of a Victauri discovery directory could not be checked (PowerShell did not answer), " +
      "so it was not trusted. Retrying shortly; where PowerShell cannot run, set victauri.port and " +
      "victauri.authToken."
    : undefined;
  if (explicit.port !== undefined) {
    const onPort = servers.filter((s) => s.port === explicit.port);
    if (explicit.token) return { ok: true, port: explicit.port, token: explicit.token };
    if (onPort.length === 1 && onPort[0].token) {
      return { ok: true, port: explicit.port, token: onPort[0].token, vouchedBy: onPort[0] };
    }
    return { ok: true, port: explicit.port, token: undefined, warning: ownerCheckWarning };
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
  const own = servers.filter(
    (s) => s.identifier !== undefined && workspaceIdentifiers.includes(s.identifier)
  );
  if (own.length === 1 || servers.length === 1) {
    const only = own.length === 1 ? own[0] : servers[0];
    const otherProject =
      own.length === 0 && workspaceIdentifiers.length > 0 && only.identifier !== undefined;
    const warning = otherProject
      ? `This workspace's app is ${workspaceIdentifiers.join(" / ")}, but the only one running is ` +
        `${only.identifier}.`
      : undefined;
    const base = only.token
      ? { ok: true as const, port: only.port, token: only.token, vouchedBy: only }
      : { ok: true as const, port: only.port, token: undefined };
    return otherProject ? { ...base, warning, otherProject } : base;
  }
  if (servers.length > 1) {
    return {
      ok: false,
      message: `Multiple Victauri apps are running: ${servers.map(label).join(", ")}. Set victauri.port to the one to connect to.`,
    };
  }
  if (ownerCheckWarning) return { ok: false, message: ownerCheckWarning };
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
