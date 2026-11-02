// A fake Victauri server that answers with what a REAL demo app answered (fixtures/record.js).
// A request nobody recorded gets a 400 and lands in `unrecorded`, so a test can assert that the
// extension only makes requests a real plugin was seen to answer.
import * as http from "node:http";
import type { AddressInfo } from "node:net";

import recorded from "./fixtures/demo-app-responses.json";

export interface Seen {
  path: string;
  auth: string | undefined;
}

export interface FakeServerOptions {
  /** Delay every `/info` answer, to simulate a busy app. */
  infoDelayMs?: number;
  /**
   * Override one tool's answer `{ status?, body }`, keyed like the fixture (`tool` or
   * `tool/action`); `/info` overrides the identifier /info reports.
   */
  override?: Record<string, any>;
}

export interface FakeServer {
  port: number;
  seen: Seen[];
  unrecorded: string[];
  override: NonNullable<FakeServerOptions["override"]>;
  close: () => Promise<void>;
}

const fixtureTools = recorded.tools as unknown as Record<string, { status: number; body: unknown }>;

export async function fakeServer(
  appIdentifier: string,
  options: FakeServerOptions = {}
): Promise<FakeServer> {
  const seen: Seen[] = [];
  const unrecorded: string[] = [];
  const override = options.override ?? {};
  const server = http.createServer((req, res) => {
    seen.push({ path: req.url ?? "", auth: req.headers.authorization });
    const reply = (body: unknown, status = 200) => {
      res.writeHead(status, { "content-type": "application/json" });
      res.end(JSON.stringify(body));
    };
    if (req.url === "/health") return reply("ok");
    if (req.url === "/info") {
      setTimeout(
        () => reply({ ...recorded.info, app_identifier: override["/info"] ?? appIdentifier }),
        options.infoDelayMs ?? 0
      );
      return;
    }
    let raw = "";
    req.on("data", (c) => (raw += c));
    req.on("end", () => {
      const tool = (req.url ?? "").replace("/api/tools/", "");
      let action: unknown;
      try {
        action = (JSON.parse(raw || "{}") as { action?: unknown }).action;
      } catch {
        action = undefined;
      }
      const key = typeof action === "string" ? `${tool}/${action}` : tool;
      const hit = override[key] ?? fixtureTools[key];
      if (!hit) {
        unrecorded.push(key);
        return reply({ error: `not recorded from a real app: ${key}` }, 400);
      }
      reply(hit.body, hit.status ?? 200);
    });
  });
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
  return {
    port: (server.address() as AddressInfo).port,
    seen,
    unrecorded,
    override,
    close: () =>
      new Promise<void>((r) => {
        server.closeAllConnections();
        server.close(() => r());
      }),
  };
}
