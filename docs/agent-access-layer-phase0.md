# bzdium — Agent Access Layer: Phase 0 Discovery & Architecture Proposal

Status: **awaiting owner confirmation** (PRD Phase 0 gate)
Audited at: v0.2.8, Tauri 2.12.1, rustc 1.98.0 (pinned), baseline `cargo test` = 37 passing.

---

## 1. What bzdium actually is (and why the PRD needs adaptation)

The PRD assumes a server-side web application with HTTP routes, a router, and request
middleware. **bzdium is none of those.** It is a single-process, single-window **Tauri 2
desktop application**:

| Property | Reality |
|---|---|
| Language / runtime | Rust 2021, `bzdium_lib` cdylib+staticlib+rlib |
| UI shell | Tauri 2 (`tauri` 2.12.1, `wry` 0.57.0) + vanilla JS (`dist/app.js`, 32 KB, no bundler, no npm deps) |
| Frontend framework | **None.** Plain DOM over `window.__TAURI__.core.invoke` |
| HTTP server | **None exists.** No axum/hyper/tiny_http dependency is pulled in for serving. |
| IPC surface | 22 `#[tauri::command]` functions over Tauri IPC (not HTTP) |
| State | In-process `Mutex<StateInner>` + JSON files on disk |
| Process model | Exactly one instance per machine, enforced by `tauri-plugin-single-instance` |
| Platforms | Windows x64 (WebView2), Linux x64 (WebKitGTK) |

**Consequence:** there is no `/api/v1/agent/` router to mount onto, and no web server to
add middleware to. The Agent Access Layer must stand up its **own loopback HTTP listener**
inside the running desktop process — which is also the safest possible place for it, because
the service logic it must reuse is only meaningful inside that process.

### Where the domain logic lives

```
src-tauri/src/
  lib.rs          22 command registrations, app setup, window lifecycle
  state.rs        Service, Settings, AppState, StateInner (the source of truth)
  storage.rs      data-dir resolution, atomic writes, corrupt-file quarantine
  services.rs     recipes (60), URL validation, TLS-bypass policy, add/update/remove/
                  reorder, icon upload, config export/import          (~1.1k lines)
  webviews.rs     per-service webview lifecycle, navigation, hibernation,
                  external-link policy, new-window handling            (~1.0k lines)
  badges.rs       report_title parsing + throttling (remote-reachable surface)
  hibernate.rs    30 s idle scan → close webviews
  tray.rs, autostart.rs, linux_layout.rs
```

### Data layout (portable)

```
bzdium-data/
  settings.json     Settings struct
  services.json     Vec<Service>
  sessions/<uuid>/  per-service WebView2/WebKitGTK profile (credentials live here)
  icons/<uuid>.<ext> uploaded custom icons
```

---

## 2. Domain capability inventory (the raw material for the tool matrix)

All 22 commands, classified for agent exposure:

| # | Command | Agent-viable | Why / notes |
|---|---|---|---|
| 1 | `get_state` | ✅ read | Full snapshot: services, active id, settings, total badges, data dir |
| 2 | `update_settings` | ⚠️ write | **Full-struct replacement** (known blast radius, review N6). Needs field-level patch wrapper. |
| 3 | `get_recipes` | ✅ read | 60 built-in service recipes |
| 4 | `add_service` | ✅ write | Validates https + TLS-bypass policy, creates webview |
| 5 | `update_service` | ✅ write | May rebuild the webview |
| 6 | `remove_service` | ✅ admin | Destroys session profile + icon files irrecoverably |
| 7 | `reorder_services` | ✅ write | Sidebar order |
| 8 | `export_config` | ⚠️ refactor | Bound to a **native save dialog** — unusable headless |
| 9 | `import_config` | ⚠️ refactor | Bound to a **native open dialog**; replaces all services |
| 10 | `pick_service_icon` | ⚠️ refactor | Native file picker |
| 11 | `reset_service_icon` | ✅ write | Pure state change |
| 12 | `get_service_icon` | ✅ read | Returns a base64 data: URL (cap the size reported to agents) |
| 13 | `set_active_service` | ✅ write | Switches the visible tab |
| 14 | `reload_service` | ✅ write | Hard reload: close webview, wait for exit, recreate |
| 15 | `navigate` | ✅ write | `back` / `forward` / `home` |
| 16 | `navigate_url` | ✅ write | Explicit https URL in that service's session |
| 17 | `hibernate_service` | ✅ write | Frees RAM |
| 18 | `wake_service` | ✅ write | Recreates the webview |
| 19 | `set_overlay_mode` | ❌ | Pure UI z-order concern |
| 20 | `open_devtools` | ❌ | Gated on `settings.dev_mode`; deliberately not agent-exposed |
| 21 | `open_service_external` | ❌ | Hands a URL to the OS browser — an escape hatch, not an agent power |
| 22 | `report_title` | ❌ | Remote-page-only badge channel; never agent-callable |

**16 of 22 commands map cleanly. 3 need a path-based twin (dialogs). 3 are excluded.**

**Zero-duplication insight:** because `#[tauri::command]` functions are ordinary Rust
functions, the HTTP layer can call `services::add_service(app, state, …)` **directly** —
no re-implementation of business logic, no second validation path, no drift. This is the
single most important finding of the audit, and it makes the PRD's "Zero-Duplication
Architecture" goal free rather than aspirational.

---

## 3. Proposed architecture

```
┌───────────────────────────── bzdium process (single instance) ─────────────────────────────┐
│                                                                                            │
│   dist/app.js ──Tauri IPC──►  22 #[tauri::command]  ──►  services / webviews / badges      │
│                                      ▲                                                     │
│                                      │ direct fn calls (no duplication)                   │
│                              agent::registry (ToolSpec)                                    │
│                               ├──► axum router  127.0.0.1:<port>  /api/v1/agent/*          │
│                               └──► rmcp server  /api/v1/agent/mcp (streamable HTTP)        │
└────────────────────────────────────────────────────────────────────────────────────────────┘
                                     ▲                    ▲
                     Authorization: Bearer              MCP stdio
                                     │                    │
                          REST / OpenAPI clients   bzdium --mcp-stdio  (thin JSON↔HTTP bridge)
```

### Why stdio is a *bridge*, not a second server

Tauri's single-instance plugin guarantees one live process, and the state those tools mutate
(webviews, active tab, in-memory service list) only exists in that process. So:

* `bzdium --mcp-stdio` starts **without** initialising Tauri: it reads the loopback port +
  token from `<dataDir>/agent/`, then proxies `tools/call` etc. over HTTP.
* If bzdium is not running, it returns a structured `BZDIUM_NOT_RUNNING` error with a
  `suggestion` telling the agent to start it (and, if `--allow-readonly`, may serve
  `services.json`/`settings.json` from disk read-only, flagged as possibly stale).
* This keeps exactly one writer for one state — the desktop app — which is what makes the
  layer deterministic.

### Proposed file tree (all new, one portable module)

```
src-tauri/src/agent/
  mod.rs         registry wiring, AgentError (code/message/suggestion), scope enum
  tools.rs       ToolSpec table → existing command fns
  resources.rs   bzdium:// resources
  prompts.rs     multi-step workflow templates
  auth.rs        token file, constant-time compare, scope enforcement
  guard.rs       dry-run previews + post-state diffing
  audit.rs       JSONL append log + rotation
  http.rs        axum router, auth middleware, health, manifest
  openapi.rs     OpenAPI 3.1 document generated from the SAME ToolSpec table
  mcp_http.rs    rmcp server mounted at /api/v1/agent/mcp
  stdio.rs       --mcp-stdio bridge
docs/AGENT_INTEGRATION.md
src-tauri/tests/agent_contract.rs
```

### Small refactors required (no behaviour change)

1. Split `export_config` / `import_config` into a **core fn taking a path** + a thin dialog
   command that calls it. Both entry points share validation, size cap, id normalisation,
   TLS downgrade, and orphan reclamation.
2. Add `import_config_from_path` core that accepts the same `ConfigFile` JSON, so agents
   can back up / restore without a human clicking a dialog.
3. Field-level `patch_settings` wrapper over `update_settings` so an agent cannot
   accidentally reset every unspecified field to a default.

---

## 4. Proposed MCP surface

### Resources (`bzdium://…`) — read-only context

| URI | Contents |
|---|---|
| `bzdium://app/state` | Full `AppState` snapshot (services, active id, settings, badges, data dir) |
| `bzdium://app/health` | Version, layout (portable/installed), webview count, uptime |
| `bzdium://services` | Service list, ordered, with runtime flags (enabled/hibernated/badge) |
| `bzdium://services/{id}` | One service |
| `bzdium://settings` | Current `Settings` |
| `bzdium://recipes` | The 60 built-in service recipes |
| `bzdium://sessions` | Per-service session dir + size on disk (never file contents) |
| `bzdium://audit/recent` | Last N agent actions (admin scope) |

### Tools

**Read (scope `read`)**
`list_services`, `get_service`, `get_settings`, `get_state`, `list_recipes`,
`get_service_icon`, `list_sessions` `(sizes only)`

**Write (scope `write`)**
`add_service`, `update_service`, `patch_settings`, `reorder_services`,
`set_active_service`, `reload_service`, `navigate` (back/forward/home),
`navigate_url`, `hibernate_service`, `wake_service`, `reset_service_icon`

**Admin (scope `admin`)**
`remove_service`, `import_config`, `export_config`

Every tool: `schemars`-derived input schema with `.describe()` on every property, a
declared `scope`, a `mutating` flag (drives `dryRun`), and a typed output.

### Prompts

| Name | Purpose |
|---|---|
| `add-messenger` | Choose a recipe → `add_service` → `set_active_service` → verify badge |
| `triage-unread` | Read `bzdium://app/state` → rank by `badgeCount` → focus the top one |
| `workspace-backup` | `export_config` to a path → verify → summarise what moved |
| `tls-bypass-audit` | List services with `ignoreTlsErrors` and explain each risk |
| `recover-hung-service` | Diagnose a wedged page → `reload_service` → verify URL |

---

## 5. Security model (the part that needs your decision)

A loopback HTTP listener inside a **browser-hosting application** is a new, real attack
surface. Any local process — and, more importantly, **any remote page loaded in any service
webview** (WhatsApp, Gmail, Discord … all run real remote JS in this process's children) —
can reach `http://127.0.0.1:<port>`. That is precisely the cross-origin class the repo's
own round-2 review already treats as High severity.

Mitigations baked into the design:

1. **Bearer token required on every request** (`Authorization: Bearer bzd_agt_…`). Custom
   headers are not sendable by a plain cross-origin `fetch` without a preflight, and the
   server answers **no** CORS preflight — so a hostile page cannot reach a tool.
2. **Reject any request carrying an `Origin` header** outright (browser-initiated), and
   require `Host: 127.0.0.1` (defeats DNS rebinding).
3. **Bind 127.0.0.1 only**, never `0.0.0.0`; ephemeral port recorded in
   `<dataDir>/agent/port` with restrictive permissions.
4. **Scopes** `read` < `write` < `admin` per token; `admin` off by default.
5. **`dryRun`** on every mutating tool: validate + compute the would-be snapshot, commit
   nothing.
6. **Audit log** JSONL with timestamp, tool, redacted args, duration, outcome, status.
7. **Service sessions are never readable through this layer** — resources expose directory
   names and sizes, never cookies or profile contents.

### Open questions for you

1. **Dependency posture.** Adding `rmcp` 3.6 (official Rust MCP SDK, + `schemars`) and
   `axum` 0.8 grows the dependency tree in a project whose whole identity is a 4–12 MB
   binary and minimal deps. Options: (a) `rmcp` + `axum` as above; (b) hand-rolled
   JSON-RPC 2.0 + a small `hyper` 1.x loopback server, zero new deps; (c) feature-gate the
   whole layer behind `--features agent` and measure the delta before committing.
   **Recommendation: (c) with (a)** — measure, then decide, and keep the layer out of the
   default build if the cost is real.
2. **Default state of the listener.** Off until the user enables it in Settings, or on by
   default whenever a token file exists? **Recommendation: off by default**, enabled by an
   explicit `agent_enabled` setting or `--agent` flag, with the token generated on first
   enable.
3. **Mutation privilege ceiling.** Should agents be able to `remove_service` and
   `import_config` (both irreversibly destroy session profiles), or should those stay
   human-only? **Recommendation: admin-only, and `import_config` additionally requires the
   confirmation token from `export_config`.**

---

## 6. Error contract

```json
{
  "error": {
    "code": "UNKNOWN_SERVICE_ID",
    "message": "No service with id 'a1b2'. 3 services are configured.",
    "suggestion": "Call list_services to get valid ids, then retry with one of them.",
    "details": { "received": "a1b2", "validCount": 3 }
  }
}
```

Codes mapped from the existing `Err(String)` messages (the audit found these exact
strings to translate): `unknown service id {id}`, `only https:// URLs are allowed`,
`TLS verification can only be disabled for self-hosted services…`, `{name} is disabled —
enable it in Edit`, `developer mode is disabled`, `unknown navigate action {a}`,
`not a bzdium config file: {e}`, `unsupported config file (type {t}/{v})`,
`config file is too large…`.

## 7. Verification plan

* Unit: schema validation per tool, scope matrix, dry-run produces no disk write,
  error-code mapping table.
* Contract: `tests/agent_contract.rs` — spin the router on an ephemeral port with a temp
  data dir, assert 401 without token, 403 on scope escalation, 400 with a
  `suggestion` on bad payload, 200 + correct body on `tools/call` and
  `POST /api/v1/agent/tools/add_service`, and that `openapi.json` validates as 3.1.
* Negative: hostile `Origin` rejected; missing `Authorization` rejected; oversized
  `import_config` rejected.
* Guard: no regression in the existing 37 tests.
