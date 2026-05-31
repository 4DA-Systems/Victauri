# Security

Victauri provides multiple layers of security to ensure that only authorized agents can access your application during development.

## Debug-Only Gate

The most fundamental security measure: **Victauri does not exist in release builds.**

```rust
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    #[cfg(debug_assertions)]
    { /* Full MCP server, JS bridge, everything */ }
    
    #[cfg(not(debug_assertions))]
    { /* Empty no-op plugin — zero binary overhead */ }
}
```

This means:
- No MCP server is started in production
- No JS bridge is injected
- No HTTP endpoints are exposed
- No memory is allocated for logs or state
- The compiled binary has zero overhead from Victauri

You cannot accidentally ship Victauri to users.

## Bearer Token Authentication

Authentication is **enabled by default**. On startup Victauri auto-generates a UUID
Bearer token and writes it to the per-process discovery directory; first-party clients read
it automatically. Localhost-only binding (`127.0.0.1`) and the `#[cfg(debug_assertions)]`
release gate are *additional* layers on top of — not a substitute for — auth, because any
other process running as the same user can also reach `127.0.0.1`.

Every request except `/health` must include a valid Bearer token.

### How It Works

1. By default the token is auto-generated — no builder call needed. Use `.auth_token("...")`
   to set a fixed value, or `.auth_disabled()` to turn auth off.
2. The token is written to the per-process discovery directory (user-only permissions) and
   auto-discovered by `VictauriClient::discover()`, the CLI, and the VS Code extension
3. Clients must include `Authorization: Bearer <token>` in every request
4. Token comparison uses constant-time equality to prevent timing attacks

### Configuration

```rust
// Auth ON by default (auto-generated UUID token, auto-discovered by clients)
VictauriBuilder::new().build()

// Fixed token
VictauriBuilder::new()
    .auth_token("my-secret-token")
    .build()

// Opt OUT of auth (you accept that any local process can connect)
VictauriBuilder::new()
    .auth_disabled()
    .build()

// Environment variable (overrides the auto-generated token with a fixed value)
// VICTAURI_AUTH_TOKEN=my-token
```

### What Is Protected

| Endpoint | Auth Required |
|----------|:------------:|
| `/health` | No |
| `/mcp` | Yes |
| `/api/tools` | Yes |
| `/api/tools/{name}` | Yes |
| `/info` | Yes |

The `/health` endpoint is unauthenticated so that the watchdog and load balancers can check liveness without credentials.

## Rate Limiting

A token-bucket rate limiter prevents abuse, even from authenticated clients:

- **Default rate:** 1000 requests per second
- **Implementation:** Lock-free `AtomicU64` counter
- **Bucket refill:** Continuous (not windowed)
- **Response on limit:** HTTP 429 Too Many Requests

This protects against runaway agents or scripts that flood the server with requests.

## Privacy Layer

Fine-grained control over what agents can see and do.

### Privacy Profiles

```rust
use victauri_plugin::PrivacyProfile;

// Read-only: agent can observe but not mutate
VictauriBuilder::new()
    .privacy_profile(PrivacyProfile::Observe)
    .build()

// Testing: can interact and record, but no arbitrary code execution
VictauriBuilder::new()
    .privacy_profile(PrivacyProfile::Test)
    .build()

// Full control (default)
VictauriBuilder::new()
    .privacy_profile(PrivacyProfile::FullControl)
    .build()
```

#### Observe Profile Disables:
- `eval_js` (arbitrary code execution)
- `screenshot` (visual data exfiltration)
- All interaction tools (click, fill, type)
- All input tools
- Storage writes
- Navigation
- CSS injection
- Recording (state capture)

#### Test Profile Disables:
- `eval_js` (arbitrary code execution)
- `screenshot`
- CSS injection

### Command Filtering

Control which Tauri commands can be invoked:

```rust
// Allowlist: only these commands can be called
VictauriBuilder::new()
    .command_allowlist(&["get_settings", "get_status"])
    .build()

// Blocklist: these commands are forbidden
VictauriBuilder::new()
    .command_blocklist(&["delete_data", "admin_reset"])
    .build()
```

### Tool Disabling

Disable individual MCP tools:

```rust
VictauriBuilder::new()
    .disable_tools(&["eval_js", "invoke_command", "screenshot"])
    .build()
```

Disabled tools:
- Return an error if called directly
- Are omitted from tool discovery listings
- Cannot be re-enabled at runtime

### Output Redaction

Automatically scrub sensitive data from all tool responses:

```rust
VictauriBuilder::new()
    .enable_redaction()
    .add_redaction_pattern(r"sk-[a-zA-Z0-9]{32,}")  // OpenAI keys
    .add_redaction_pattern(r"ghp_[a-zA-Z0-9]{36}")   // GitHub tokens
    .build()
```

Built-in patterns (when redaction is enabled):
- API key values in JSON (`"api_key": "..."` becomes `"api_key": "[REDACTED]"`)
- Bearer tokens in strings
- Email addresses
- Common secret key formats

Redaction is applied as a post-processing step to all tool output, regardless of which tool generated it.

## Origin Guard

The MCP server only accepts connections from localhost (`127.0.0.1` / `::1`). The axum server binds exclusively to `127.0.0.1`, meaning:

- No remote network access is possible
- Other machines on the LAN cannot connect
- Only processes on the same machine can reach the server

For the Chrome extension (`victauri-browser`), an additional origin guard rejects requests with non-localhost `Origin` headers, preventing web pages from connecting to the native host.

## Security Headers

All HTTP responses include security headers:

- `X-Content-Type-Options: nosniff`
- `X-Frame-Options: DENY`
- `Cache-Control: no-store`

## Threat Model

### What Victauri Protects Against

| Threat | Mitigation |
|--------|-----------|
| Production exposure | `#[cfg(debug_assertions)]` gate |
| Unauthorized local access | Bearer token auth (**on by default**) + localhost-only binding |
| Timing attacks on auth | Constant-time comparison |
| Request flooding | Token-bucket rate limiter |
| Remote network access | Localhost-only binding |
| Data exfiltration | Privacy profiles + output redaction |
| Dangerous mutations | Tool disabling + command allowlists |
| Cross-origin attacks | Origin header validation |

### What Is Out of Scope

- **Malicious code on the same machine with the auth token** — If an attacker has the token and localhost access, they have the same privileges as the legitimate agent. This is inherent to any localhost-based development tool.
- **Memory inspection of the process** — A sufficiently privileged attacker on the same machine could read process memory directly. Victauri does not add encryption at rest for in-process data.

## Recommendations

For typical development (auth on by default — token auto-generated and auto-discovered):
```rust
VictauriBuilder::new().build()
```

For CI/automated testing:
```rust
// Fixed token from environment
VictauriBuilder::new()
    .auth_token(std::env::var("CI_VICTAURI_TOKEN").unwrap())
    .build()
```

For shared development environments:
```rust
// Auth + restrictive privacy
VictauriBuilder::new()
    .auth_enabled()
    .privacy_profile(PrivacyProfile::Observe)
    .command_blocklist(&["dangerous_admin_command"])
    .build()
```
