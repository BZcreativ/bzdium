# bzdium — Design Spec

Date: 2026-10-08
Status: Approved by user direction ("Option C — Full Rust port of Ferdium, call it bzdium", motivation: Ferdium/Electron is heavy and crashes; want a light, stable app)

## 1. Intent

A from-scratch Rust re-implementation of Ferdium's core concept for Windows x64:
one window hosting many messaging-service webviews (WhatsApp, Telegram, Slack,
Discord, Gmail, …) behind a sidebar, shipped as a **single portable exe** that
uses the system WebView2 runtime instead of bundling Chromium.

Success criteria:

- Single portable `bzdium.exe` for Windows x64 (no installer, no bundled Chromium,
  data stored next to the exe).
- Multiple services, each with a persistent isolated session (survives restarts).
- Sidebar tab switching, unread-count badges, service hibernation for low RAM.
- Noticeably lighter than Electron Ferdium (target: < 30 MB exe, no Chromium in the bundle).

Non-goals for v1 (deliberate YAGNI cuts vs Ferdium): Ferdium-server account sync,
workspaces, todos, 200+ recipe catalog with per-service JS hacks, i18n, auto-updater,
macOS/Linux builds, screen sharing / video-call tuning.

## 2. Architecture

Tauri v2 application. One OS window containing:

- **UI webview** ("chrome"): the sidebar + dialogs, plain HTML/CSS/JS, no framework,
  no bundler — served from `dist/` via Tauri's embedded assets.
- **Service webviews**: one child webview per *active* service, positioned to the
  right of the sidebar, each with its own WebView2 user-data folder
  (`<data>/sessions/<service-id>/`) for cookie/session isolation and persistence.

Why Tauri multi-webview instead of iframe-style tabs: each service gets a real,
isolated browser context (Ferdium's Electron `partition` equivalent), and closing a
webview actually frees its memory (this is what makes hibernation work).

### Process/data layout (portable)

```
bzdium.exe
bzdium-data/
  settings.json        # app settings
  services.json        # configured services (id, name, url, icon, order, hibernated)
  sessions/<id>/       # per-service WebView2 user data (cookies, storage)
```

Data dir resolution: `<exe_dir>/bzdium-data` if creatable/writable, else
`%APPDATA%/bzdium` (fallback keeps the app usable when run from read-only media).

### Rust modules (`src-tauri/src/`)

- `main.rs` — entry point, window + UI webview setup, tray, command registration.
- `state.rs` — `AppState` (services, settings, active service, last-active
  timestamps), guarded by `Mutex`.
- `storage.rs` — load/save `settings.json` / `services.json`, data-dir resolution,
  atomic writes (write-temp-then-rename; crash safety is an explicit goal).
- `services.rs` — service CRUD + reorder commands; built-in recipe catalog
  (WhatsApp, Telegram, Slack, Discord, Messenger, Gmail, Outlook, Google Chat,
  Teams, Matrix/Element, Signal, X, LinkedIn, Instagram + "custom URL").
- `webviews.rs` — create/show/hide/close service webviews, bounds management on
  window resize, navigation commands (reload, back/forward, home).
- `hibernate.rs` — background task: webviews idle > N minutes (default 30,
  configurable) are closed; wake recreates the webview at the service URL.
- `badges.rs` — per-webview injected script polls `document.title`, extracts
  unread counts (`(3)`, `•`, etc.) and invokes a command; badge state aggregated
  and pushed to the UI; window title + badge reflect total.
- `tray.rs` — system tray: show/hide window, quit; minimize-to-tray option.

### IPC contract

Commands (UI → Rust): `get_state`, `add_service`, `update_service`,
`remove_service`, `reorder_services`, `set_active_service`, `reload_service`,
`navigate` (back/forward/home), `hibernate_service`, `wake_service`,
`get_settings`, `update_settings`, `report_title` (from service webviews).

Events (Rust → UI): `state-changed` carrying the full serializable app state
(services with badge counts + hibernated flag, settings). The UI re-renders from
this single event — one source of truth, no drift.

### UI (dist/)

- `index.html`, `styles.css`, `app.js` — no build step.
- Sidebar: service icons (emoji/SVG lettermark fallback), unread badges,
  hibernation indicator; click to switch; context menu (reload, hibernate, edit,
  remove); "+" opens Add-Service dialog (catalog grid + custom form).
- Settings dialog: hibernation timeout, minimize-to-tray, start-with-Windows
  (registry Run key), dark UI toggle.
- Window resize keeps the active service webview bounds in sync
  (Rust listens to resize events; sidebar width constant 72 px).

## 3. Error handling

- Storage: atomic writes; corrupt JSON → back up to `*.broken-<timestamp>` and
  start from defaults rather than crash.
- WebView creation failure (e.g. missing runtime): show a blocking dialog with
  the WebView2 download link.
- Any command error is returned as `Err(String)` and surfaced in the UI as a
  toast — the app must never panic on user action.

## 4. Testing

- Rust unit tests: badge-count title parsing, settings/services serde round-trip,
  data-dir resolution fallback, atomic-write recovery from corrupt files.
- Manual smoke test: build `--release`, launch exe, add WhatsApp + Telegram +
  custom site, switch tabs, verify badge from title change, hibernate (short
  timeout in settings), restart app, verify sessions persisted.

## 5. Build

`cargo build --release --target x86_64-pc-windows-msvc` →
`src-tauri/target/x86_64-pc-windows-msvc/release/bzdium.exe` — the portable
artifact (requires WebView2 runtime, present on Windows 10/11 with Edge).
No NSIS/MSI bundling for v1.

## 6. IPC Contract (normative — backend and frontend MUST match exactly)

All commands are Tauri commands invoked from JS as
`window.__TAURI__.core.invoke(<name>, <args>)`. `withGlobalTauri` is enabled.
Arguments are camelCase (Tauri converts to snake_case parameters automatically).

### Types (JSON)

```ts
type ServiceId = string; // uuid v4, lowercase hex with dashes

interface Service {
  id: ServiceId;
  name: string;
  url: string;           // full https URL, validated
  icon: string;          // emoji char OR single letter; UI renders lettermark
  enabled: boolean;
  order: number;         // 0-based position in sidebar
  hibernated: boolean;   // runtime: webview currently closed
  badgeCount: number;    // runtime: unread count parsed from title
}

interface Settings {
  hibernationMinutes: number;   // 0 = never hibernate; default 30
  minimizeToTray: boolean;      // default true
  startWithWindows: boolean;    // default false
  darkUi: boolean;              // default true
}

interface AppState {
  services: Service[];          // sorted by order
  activeServiceId: ServiceId | null;
  settings: Settings;
  totalBadgeCount: number;
  dataDir: string;              // resolved portable data dir
}
```

### Commands (UI → Rust)

| Command | Args | Returns | Effect |
|---|---|---|---|
| `get_state` | — | `AppState` | Snapshot; UI calls once at startup. |
| `add_service` | `{ name: string, url: string, icon: string }` | `AppState` | Validates URL (https only), creates service, saves, activates it. |
| `update_service` | `{ id: ServiceId, name: string, url: string, icon: string, enabled: boolean }` | `AppState` | Edits service; if URL changed, webview is recreated. |
| `remove_service` | `{ id: ServiceId }` | `AppState` | Closes webview, deletes service (NOT its session folder). |
| `reorder_services` | `{ orderedIds: ServiceId[] }` | `AppState` | Sets order by array position. |
| `set_active_service` | `{ id: ServiceId \| null }` | `AppState` | Shows that service's webview (waking it if hibernated), hides others. |
| `reload_service` | `{ id: ServiceId }` | `AppState` | Reloads the webview (wakes if hibernated). |
| `navigate` | `{ id: ServiceId, action: "back" \| "forward" \| "home" }` | `AppState` | History nav or loads service home URL. |
| `hibernate_service` | `{ id: ServiceId }` | `AppState` | Closes the webview, marks hibernated. |
| `wake_service` | `{ id: ServiceId }` | `AppState` | Recreates the webview. |
| `update_settings` | `{ settings: Settings }` | `AppState` | Persists settings, applies side effects (autostart key, hibernation timer). |
| `report_title` | `{ id: ServiceId, title: string }` | `null` | Called BY service webviews (injected script); updates badge. Does NOT emit state-changed unless the badge count changed. |

Every command that returns `AppState` also emits a `state-changed` event
(payload: `AppState`) on the **UI webview only** after mutating state; the UI
re-renders purely from events (plus the initial `get_state`).

UI listens via `window.__TAURI__.event.listen("state-changed", e => render(e.payload))`.

### Service webview injection (backend-owned)

Each service webview is created with an initialization script that, every 3 s,
reads `document.title` and calls
`window.__TAURI__.core.invoke("report_title", { id: "<service-id>", title })`.
Badge parsing (Rust side): first match of `/\((\d+)\)/` → that number; title
starting with `•` or containing `(•)` → count 1; otherwise 0.

### Window geometry contract

- Sidebar width: **72 px** constant. Window default size 1280×800.
- Active service webview bounds: `{ x: 72, y: 0, width: winW - 72, height: winH }`
  in logical pixels; hidden services get `set_visible(false)` (bounds retained).
- Rust updates bounds on every window resize event.
