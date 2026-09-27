// Discovery of a running Victauri app: its port and Bearer token, from the per-process
// directories the plugin writes. Mirrors `victauri-test::discovery` (the reference
// implementation); kept free of `vscode` imports so it can be exercised under plain Node.
import * as os from "os";
import * as path from "path";
import * as fs from "fs/promises";

export interface DiscoveredServer {
  port: number;
  token: string | undefined;
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

// Whether `pid` is a live process OWNED BY US. `process.kill(pid, 0)` sends no signal; it
// succeeds only for a process we may signal. EPERM means the process exists but belongs to
// another user — which, for a discovery entry, means its PID was recycled by someone else's
// process: never alive for our purposes (the cross-user PID-reuse class, audit R2-7).
export function pidIsAlive(pid: number): boolean {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function readServer(dir: string): Promise<DiscoveredServer | undefined> {
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
    return { port, token };
  } catch {
    return undefined; // no port file in this dir
  }
}

// The single live, trusted app across all roots (a PID found in two roots counts once), or
// the default port with NO token when there is none or more than one.
export async function discoverServer(defaultPort: number): Promise<DiscoveredServer> {
  const byPid = new Map<string, DiscoveredServer>();
  for (const root of await discoveryRoots()) {
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
      // Skip dirs left behind by an exited app (a crash / kill leaves them on disk):
      // one stale dir next to the live one would make discovery ambiguous.
      if (!pidIsAlive(parseInt(entry.name, 10))) continue;
      const server = await readServer(dir);
      if (server) byPid.set(entry.name, server);
    }
  }
  const servers = [...byPid.values()];
  if (servers.length === 1) return servers[0];
  return { port: defaultPort, token: undefined };
}
