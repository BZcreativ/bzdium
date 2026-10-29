# bzdium — Agent Integration Guide

bzdium exposes an **Agent Access Layer**: a single, schema-first interface that is projected onto two transports.

| Transport | Entry point | For |
| :--- | :--- | :--- |
| **MCP** (Model Context Protocol) | `bzdium --mcp-stdio`, or streamable HTTP at `POST /api/v1/agent/mcp` | Claude Desktop, Cursor, and any MCP client |
| **REST / OpenAPI 3.1** | `/api/v1/agent/*` + `/api/v1/agent/openapi.json` | OpenAI Actions, LangGraph, custom HTTP tool callers |

Both transports serve **the exact same tool registry**. There is no second implementation of any domain rule: every tool calls the same Rust function the desktop UI invokes over Tauri IPC.

---

## 1. Enabling the layer

The Agent Access Layer is **off by default**. Nothing listens until a human turns it on, and two separate switches must agree before a socket is bound.

1. Open bzdium → **Settings** → tick **Enable Agent Access API (MCP + REST)**.
2. bzdium immediately mints a **write-scoped** key and shows it in the Agent Access panel. **Copy it now** — the value is displayed only once. It is stored in `<dataDir>/agent/tokens.json` so it survives restarts; that file (and the `agent/` directory) is restricted to the current user on platforms with file permissions (0600/0700 on Linux).
3. The loopback listener starts on `127.0.0.1` and writes its port to `<dataDir>/agent/port`.

No restart is needed: enabling and disabling both take effect on the next request.

### The two switches

The layer obeys **both** of these, and they are deliberately separate files:

| Switch | File | Meaning |
| :--- | :--- | :--- |
| **Settings checkbox** | `settings.json` → `agentAccess` | What the human sees. Mirrored into the layer config. |
| **Layer config** | `<dataDir>/agent/config.json` → `enabled` | What the listener obeys. |

`update_settings` writes both. Startup refuses to listen unless both are on (it logs once and stays off). This is why editing `settings.json` by hand is not enough: use the checkbox.

### Enabling and disabling live

Disabling does **not** unbind the socket. The listener stays bound for the lifetime of the process and a process-wide gate answers `503 Service Unavailable` to every request while the layer is off:

```jsonc
{
  "error": {
    "code": "NOT_RUNNING",
    "message": "The bzdium Agent Access layer is currently switched off.",
    "suggestion": "Enable Agent Access in bzdium Settings."
  }
}

This is intentional. Tearing down a listener mid-request leaves half-closed state and can fail in the unsafe direction; a closed gate is instant, atomic, and fails closed. The check happens **before** authentication, so a switched-off layer reveals nothing — not a route list, not whether a token is valid. The port is released when the process exits.

### Admin scope is a ceiling, not a grant

Tick **Allow admin-scope tools** in the panel to make admin tokens *possible*. It does not create one, and it never upgrades an existing key:

- The bootstrap key minted at step 2 is **always `write`-scoped**, even if the ceiling is open.
- An `admin` key must be requested explicitly (the **New key** control, or the `mint_agent_token` command with `scope: "admin"`), and is refused while the ceiling is closed.
- **Unticking the box disables admin keys that already exist.** The ceiling is checked when a request is authorised, not only when a key is minted, so closing it returns `403 FORBIDDEN_SCOPE` to an admin token on its very next call. Removing admin privilege must actually remove it.

Admin tools can delete services and **replace the user's entire configuration**, so granting that power is a separate, deliberate act.

### Managing keys

| Action | Where |
| :--- | :--- |
| View status | `get_agent_status` — enabled, running, port, `baseUrl`, token count. **Never a token value.** |
| Mint a key | **New key** button → `mint_agent_token` |
| Revoke all | **Revoke all** button → `revoke_agent_tokens`. Takes effect on the **next request**; no restart needed. |
| Allow admin | **Allow admin-scope tools** → `set_agent_admin_scope` (asks for confirmation) |

### Environment variables

| Variable | Used by | Meaning |
| :--- | :--- | :--- |
| `BZDIUM_AGENT_API_KEY` | `--mcp-stdio`, HTTP clients | The bearer token. Must be `bzd_agt_`-prefixed; control characters (CR/LF) are refused. |
| `BZDIUM_AGENT_PORT` | `--mcp-stdio` | Override the auto-discovered port — e.g. to target one of several running copies. |

### Data directory

| Path | Contents |
| :--- | :--- |
| `<dataDir>/agent/config.json` | Layer configuration (enabled, port, rate limit) |
| `<dataDir>/agent/tokens.json` | Minted tokens and their scopes |
| `<dataDir>/agent/port` | Port of the running listener |
| `<dataDir>/agent/audit.jsonl` | Append-only audit log of every agent action |

`<dataDir>` is `bzdium-data` beside the exe for portable builds, otherwise the platform app-data directory.

---

## 2. Security model

A loopback listener inside an app that hosts remote web content is a real attack surface. Every remote page running in a service webview (WhatsApp, Gmail, Discord…) executes attacker-influenceable JavaScript in this process's children. The layer therefore enforces, on **every** request:

| Control | Behaviour |
| :--- | :--- |
| **Loopback bind** | `127.0.0.1` only — never `0.0.0.0`. |
| **Origin rejection** | Any request carrying an `Origin` header is rejected with 403. A page cannot reach the API even if it guesses the port. |
| **Host validation** | `Host` must be `127.0.0.1[:port]` or `localhost[:port]`. Defeats DNS rebinding. |
| **Bearer token** | `Authorization: Bearer bzd_agt_…`, compared in constant time. |
| **No CORS** | `OPTIONS` is refused; no preflight is ever satisfied. |
| **Rate limit** | Per-token, default 120 requests/minute. |
| **Scopes** | `read` < `write` < `admin`. `admin` tools are unreachable unless the ceiling is explicitly opened **and** the token was minted at `admin`. |
| **No ambient admin** | The bootstrap key is always `write`-scoped; opening the admin ceiling never upgrades an existing or first token. |
| **Live off switch** | A process-wide gate answers 503 before authentication, so a disabled layer leaks nothing. |
| **Live revocation** | The token file is re-read on every request, so a revoked key stops working at once. A deleted or unreadable token file authenticates nobody. |
| **Audit** | Every call logged with timestamp, tool, redacted args, duration, outcome. |

**Session profiles are never exposed.** `list_sessions` and `bzdium://sessions` return directory names and byte sizes only — never cookies, tokens, or local storage.

---

## 3. Claude Desktop / Cursor (MCP over stdio)

`bzdium --mcp-stdio` runs a **stdio↔HTTP bridge**. It deliberately does **not** start a second Tauri instance (which would fight the single-instance lock); it forwards to the already-running bzdium.

`claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "bzdium": {
      "command": "C:\\Program Files\\bzdium\\bzdium.exe",
      "args": ["--mcp-stdio"],
      "env": {
        "BZDIUM_AGENT_API_KEY": "bzd_agt_your_token_here"
      }
    }
  }
}
```

For Cursor, the same block goes in `.cursor/mcp.json` (project) or the global Cursor MCP settings.

For a portable build, point `command` at the portable `bzdium.exe` next to `bzdium-data`.

> If bzdium is not running, the bridge returns a `BZDIUM_NOT_RUNNING` error whose `suggestion` tells the calling agent to start the app first.

---

## 4. REST quickstart

```bash
# Discover the port
PORT=$(cat "$BZDIUM_DATA_DIR/agent/port")
BASE="http://127.0.0.1:$PORT/api/v1/agent"
TOKEN="bzd_agt_your_token_here"

# Health
curl -s "$BASE/health" -H "Authorization: Bearer $TOKEN"

# What can this agent do?
curl -s "$BASE/manifest" -H "Authorization: Bearer $TOKEN"

# OpenAPI 3.1 document (feed this to a function-calling client)
curl -s "$BASE/openapi.json" -H "Authorization: Bearer $TOKEN"

# List tools and their schemas
curl -s "$BASE/tools" -H "Authorization: Bearer $TOKEN"

# Call a read tool
curl -s -X POST "$BASE/tools/list_services" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{}'

# Preview a mutation without committing it
curl -s -X POST "$BASE/tools/add_service" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"Work Slack","url":"https://app.slack.com","dryRun":true}'

# Read a resource
curl -s "$BASE/resources/services" -H "Authorization: Bearer $TOKEN"

# Render a workflow prompt
curl -s "$BASE/prompts/triage-unread" -H "Authorization: Bearer $TOKEN"
```

---

## 5. Tools

Every tool declares a JSON Schema with a description on **every** property, so an LLM can populate it without guessing. `supportsDryRun` marks tools that accept `dryRun: true`.

### Read scope

| Tool | Purpose |
| :--- | :--- |
| `list_services` | Services with id, name, url, enabled/hibernated, badge count, active id |
| `get_service` | One service by id |
| `get_settings` | Current application settings |
| `get_state` | Full snapshot (services, active id, settings, badges, data dir) |
| `list_recipes` | 60 built-in service recipes |
| `get_service_icon` | Custom icon as a data URL, or null |
| `list_sessions` | Session profile sizes on disk — **names and bytes only** |

### Write scope

| Tool | Purpose | Dry-run |
| :--- | :--- | :--- |
| `add_service` | Add a service and open its isolated session | yes |
| `update_service` | Update name/url/icon/enabled (url change rebuilds the webview) | yes |
| `patch_settings` | **Field-level** settings merge — unspecified fields keep their values | yes |
| `reorder_services` | Set sidebar order | yes |
| `set_active_service` | Focus a service | yes |
| `reload_service` | **Hard** reload: close + recreate the webview | yes |
| `navigate` | `back` / `forward` / `home` | yes |
| `navigate_url` | Load an explicit https URL in that session | yes |
| `hibernate_service` | Close the webview, keep the login | yes |
| `wake_service` | Recreate a hibernated webview | yes |
| `reset_service_icon` | Drop the custom icon | yes |

### Admin scope

| Tool | Purpose | Notes |
| :--- | :--- | :--- |
| `remove_service` | Permanently delete a service | **Deletes the session profile — logins are unrecoverable** |
| `export_config` | Write services+settings to a path | Returns a `confirmationToken` |
| `import_config` | **Replace** all services+settings | Requires the `confirmationToken` from `export_config`; valid 10 minutes |

Admin scope is **disabled by default**, and it is a two-part gate: the ceiling must be open (`allow_admin_scope`) *and* the calling token must itself be `admin`-scoped. A `write` token gets `403 FORBIDDEN_SCOPE` even with the ceiling open. `remove_service` and `import_config` are irreversible, which is why neither is reachable by the bootstrap key.

### Deliberately not exposed

`set_overlay_mode` (pure UI z-order), `open_devtools` (gated behind Developer mode), `open_service_external` (OS escape hatch), `report_title` (remote-page badge channel), `pick_service_icon` (native file dialog). These are UI-only or security-gated and must not become agent powers.

---

## 6. Resources (MCP `bzdium://…`, REST `GET /resources/<path>`)

| URI | Contents |
| :--- | :--- |
| `bzdium://app/state` | Full application snapshot |
| `bzdium://app/health` | Status, version, uptime, service/session counts |
| `bzdium://services` | Ordered service list |
| `bzdium://services/{id}` | One service |
| `bzdium://settings` | Settings object |
| `bzdium://recipes` | Built-in recipes |
| `bzdium://sessions` | Session directory sizes only |
| `bzdium://audit/recent` | Recent agent actions |

---

## 7. Prompts (MCP prompts, REST `GET /prompts/<name>`)

| Prompt | Purpose |
| :--- | :--- |
| `add-messenger` | Pick a recipe → `add_service` → focus → verify |
| `triage-unread` | Rank services by badge count and focus the busiest |
| `workspace-backup` | `export_config` and verify the file |
| `tls-bypass-audit` | Report services with TLS verification disabled and why it matters |
| `recover-hung-service` | Diagnose a wedged page and hard-reload it |

---

## 8. Error contract

Every failure returns an actionable envelope. The `suggestion` field exists so a calling LLM can **correct itself** without a human in the loop.

```json
{
  "error": {
    "code": "UNKNOWN_SERVICE_ID",
    "message": "No service is configured with id a1b2.",
    "suggestion": "Call list_services to retrieve valid service ids, then retry with one of them.",
    "details": { "received": "a1b2" }
  }
}
```

| Code | HTTP | Meaning |
| :--- | :--- | :--- |
| `UNAUTHORIZED` | 401 | Missing or bad bearer token |
| `FORBIDDEN_SCOPE` | 403 | Token scope too low for this tool |
| `RATE_LIMITED` | 429 | Per-token rate limit exceeded |
| `UNKNOWN_TOOL` / `UNKNOWN_RESOURCE` / `UNKNOWN_PROMPT` | 404 | Name not registered |
| `INVALID_PARAMETER` | 400 | Schema violation; `details.field` names it |
| `UNKNOWN_SERVICE_ID` | 400 | No such service |
| `INVALID_URL` | 400 | URL missing or not https |
| `TLS_BYPASS_REFUSED` | 400 | TLS bypass attempted on a built-in public service |
| `SERVICE_DISABLED` | 400 | Operation needs an enabled service |
| `INVALID_NAV_ACTION` | 400 | Not back/forward/home |
| `CONFIG_INVALID` | 400 | Not a valid bzdium config |
| `CONFIG_TOO_LARGE` | 400 | Config exceeds 2 MiB |
| `CONFIRMATION_REQUIRED` | 400 | `import_config` needs a fresh `export_config` token |
| `INTERNAL_ERROR` | 500 | Unexpected failure inside bzdium |
| `NOT_RUNNING` | 503 | The layer is switched off in Settings. Checked before authentication, so it leaks nothing. |
| `BZDIUM_NOT_RUNNING` | 500 | stdio bridge could not reach the app |

---

## 9. Dry-run

Mutating tools accept `dryRun: true` — with two deliberate exceptions: `export_config` and `import_config` (their side effects are a file write / a full config replacement, which cannot be previewed meaningfully, so they reject the flag). A dry run runs **the same validators as the real call** and returns the projected effect without touching state. It is a genuine precondition check, not a mock: a preview that succeeds tells the agent the real call will pass validation.

```bash
curl -s -X POST "$BASE/tools/remove_service" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"id":"<uuid>","dryRun":true}'
```

```json
{
  "dryRun": true,
  "tool": "remove_service",
  "applied": false,
  "wouldApply": {
    "wouldRemove": "<uuid>",
    "remaining": 2,
    "irreversible": "The service session profile (saved logins) would be permanently deleted."
  },
  "note": "Nothing was changed. Re-issue with dryRun: false to commit."
}
```

---

## 10. Implementation notes

* **Zero new dependencies.** The HTTP server is built on hyper 1.x, which was already resolved transitively via Tauri/wry. No axum, no rmcp — both protocol surfaces are implemented directly against their specs, keeping the binary and supply-chain surface unchanged.
* **Zero domain duplication.** Handlers call `services::*` / `webviews::*` directly. The agent and the UI share one validation path, so they cannot disagree about what is legal.
* **Off by default, fail closed.** A missing or corrupt `config.json` resolves to disabled.
* **Single writer.** The MCP stdio bridge forwards to the running desktop process rather than starting a second one, so exactly one process ever owns the state.
