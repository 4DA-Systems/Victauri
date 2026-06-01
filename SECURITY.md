# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.5.x   | Yes       |
| 0.4.x   | Yes       |
| 0.3.x   | No        |
| 0.2.x   | No        |
| 0.1.x   | No        |

## Reporting a Vulnerability

**Do not report security vulnerabilities through public GitHub issues.**

Email **hello@4da.ai** with:

- Description of the vulnerability
- Steps to reproduce
- Affected version(s)
- Impact assessment (if known)

### Response Timeline

- **48 hours** — acknowledgment of your report
- **7 days** — initial assessment and severity classification
- **30 days** — fix developed and coordinated disclosure

We follow coordinated disclosure. We will credit reporters unless they prefer anonymity.

## Scope

### In Scope

- The embedded MCP server (axum on `127.0.0.1:7373`)
- DNS rebinding attacks against the localhost server
- Origin validation and authentication bypass
- Unauthorized access to webview, IPC, or backend state via MCP tools
- Information disclosure through the MCP interface
- The authentication middleware (Bearer token validation)

### Out of Scope

- **Tauri core vulnerabilities** — report these to the [Tauri security team](https://github.com/tauri-apps/tauri/security/policy)
- **Application-level business logic** — vulnerabilities in apps that use Victauri as a dependency are the responsibility of those app authors
- **Release builds** — all Victauri plugin code is gated behind `#[cfg(debug_assertions)]` and does not exist in release binaries
- **Denial of service against localhost** — the MCP server is intentionally a debug-only, local-only interface

## Security Design

Victauri is a **debug-only development tool**. Key security properties:

- All plugin code is compiled out in release builds (`#[cfg(debug_assertions)]`)
- The MCP server binds exclusively to `127.0.0.1` (not `0.0.0.0`)
- Bearer token authentication is **enabled by default** (auto-generated token) and protects every endpoint except `/health`
- The `/health` endpoint is unauthenticated by design (for watchdog polling)

## Contact

4DA Systems Pty Ltd (ACN 696 078 841)
Email: hello@4da.ai
