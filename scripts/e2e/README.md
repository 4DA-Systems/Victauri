# Victauri End-to-End Test Harness

REST-driven E2E suites that exercise all 31 MCP tools against a **running Tauri
app** with the `victauri-plugin` embedded. They hit the dual-protocol REST API
(`POST /api/tools/{name}`) on `http://127.0.0.1:7373`, so no MCP handshake is
needed and the scripts stay dependency-light (`curl` + `jq`).

These were built and validated against [4DA](https://github.com/runyourempire)
— a real, complex Tauri 2 app (3 windows, 383 IPC commands, a 300 MB SQLite DB,
heavy network traffic) — which surfaces edge cases that synthetic demo apps do not.

## Scripts

| Script | Focus |
|---|---|
| `01-exhaustive-core.sh` | Happy-path coverage of every tool + server infra, concurrency, rapid-fire (≈190 assertions). |
| `02-exhaustive-extended.sh` | Deep parameter variations, multi-window, fault-injection E2E, contract testing, MCP protocol resources (≈125 assertions). |
| `03-adversarial-limits.sh` | Adversarial / limits / weakness probe: eval edge cases, SQL injection & read-only enforcement, path traversal, dangerous URLs, malformed input, security guards. Records raw responses for manual scrutiny. |

## Prerequisites

1. A Tauri app with `victauri-plugin` running and its MCP server reachable on
   `127.0.0.1:7373` (the scripts assume `auth_disabled()` — for an
   auth-enabled server, add `-H "Authorization: Bearer <token>"` to the `tool()`
   helpers, or read the token from `<temp>/victauri/<pid>/token`).
2. `curl` and `jq` on PATH.

## Running

```bash
# individually
bash scripts/e2e/01-exhaustive-core.sh
bash scripts/e2e/02-exhaustive-extended.sh
bash scripts/e2e/03-adversarial-limits.sh   # writes scripts/adversarial-results.txt

# or all of them
bash scripts/e2e/run-all.sh
```

## Notes

- The exhaustive suites print `PASS:`/`FAIL:` per assertion and a final tally.
- The adversarial script is **descriptive, not pass/fail** — read the recorded
  responses to judge behaviour (it deliberately probes failure modes).
- "Failures" in the exhaustive suites are usually DOM-ref instability between
  snapshots (correct behaviour) or actionability enforcement (correct). Always
  read the actual response before treating a `FAIL:` as a bug.
- The suites are app-agnostic in structure but a few assertions reference 4DA
  specifics (window labels, command names); adapt those for other apps.
