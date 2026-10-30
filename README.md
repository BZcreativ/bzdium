# bzdium

**A lightweight, portable multi-messenger for Linux and Windows x64 — the [Ferdium](https://github.com/ferdium/ferdium-app) concept, re-implemented from scratch in Rust.**

<video src="https://github.com/BZcreativ/bzdium/releases/download/v0.2.1/bzdium-demo.mp4" controls muted width="640"></video>

*(15-second overview — [docs/demo.mp4](docs/demo.mp4) in this repo)*

One window, all your messengers: WhatsApp, Telegram, Discord, Slack, Gmail and 50+ more as real, isolated web apps — with unread badges, hibernation for low RAM, and a single portable binary. No Electron, no bundled Chromium: bzdium uses the native system webview runtime (WebKitGTK on Linux, WebView2 on Windows).

> **Relationship to Ferdium:** bzdium is an independent, from-scratch re-implementation of the Ferdium *concept* (one window, many messaging services, per-service session isolation, unread badges, hibernation). It contains **no Ferdium code** — the original is Electron/JavaScript and licensed AGPL-3.0; bzdium is Rust + [Tauri](https://v2.tauri.app) + vanilla JS. All credit for the original idea and UX goes to the Ferdium team and its Ferdi/Franz ancestors.

## Why

Ferdium is a great idea in a heavy package: full Chromium per app, hundreds of MB of RAM at idle, and for this author, crashes. bzdium keeps the idea and drops the weight:

| | Ferdium (Electron) | bzdium (Rust / Native Webview) |
|---|---|---|
| Bundle | ~100 MB installer, bundled Chromium | single **~4–12 MB** binary, system WebKitGTK / WebView2 |
| Idle RAM | 300–600+ MB | **~30 MB** |
| Per-service isolation | Chromium partitions | isolated browser profiles under `bzdium-data/sessions/` |
| Recipes | 700+ (per-service JS hacks) | 60 popular web clients + any custom https URL |
| Crash safety | — | atomic JSON writes, corrupt-file quarantine |
| Platforms | Windows, macOS, Linux | Linux x64, Windows 10/11 x64 |

Measured on Windows 11 & Linux, 2 live messenger webviews.

## Features

- **Portable binary & native packages** — data lives in `bzdium-data/` next to the binary (falls back to `~/.local/share/bzdium` on Linux or `%APPDATA%\bzdium` on Windows for read-only installations)
- **62 built-in services** (messaging, mail, social, AI, productivity) + custom URLs for anything self-hosted
- **Grok, two ways** — "Grok" opens grok.com like any other service; "Grok API" is a native assistant that chats with xAI's Grok models over the official API with streaming replies, cancellation, local conversation history and a live model picker. The API key is stored encrypted with Windows DPAPI, never shown in full, and is billed by xAI separately from any Grok website subscription (see [docs/grok-integration.md](docs/grok-integration.md)). API mode requires Windows; website mode works everywhere.
- **Homelab & self-hosted support** — option to trust invalid / self-signed TLS certificates with clear security disclaimers
- **Persistent isolated sessions** — each service gets its own browser profile; logins survive restarts
- **Unread badges** parsed from page titles (capped, spoof-resistant: a page can only ever report its own count)
- **Hibernation** — idle services' webviews are closed after N minutes and wake on click, freeing RAM
- **Stuck-page watchdog (on by default)** — webviews that stop answering liveness probes while the machine is online (wedged network stack — seen with Gmail on WebView2 156) are hard-reloaded automatically, capped at 2 reloads per service per 30 minutes; a toast explains every intervention. Toggle: Settings → "Auto-recover stuck pages"
- **WebView2 runtime pin (Windows)** — Settings → "WebView2 runtime" lets you pin every webview to a specific installed runtime (e.g. hold 155 while a fresh Evergreen build is wedging pages); invalid/removed folders fall back to the system runtime at startup
- **URL bar** with live URL display, visibility toggle, and Home button
- **External link control** — open in OS default browser (via `xdg-open` / `ShellExecuteW`) or inside an isolated in-app bzdium window
- **Workspace export / import** — settings + services as a JSON file (service ids preserved, so local sessions reconnect)
- **Agent Access API (opt-in, off by default)** — a loopback-only MCP + REST/OpenAPI surface over the same commands the UI uses, so a local AI agent can drive bzdium; enable it explicitly in Settings → "Enable Agent Access API"
- Dark/light UI, system tray + minimize-to-tray, drag-to-reorder, single-instance

## Install

### Linux x64

Download from the [Latest Release (v0.2.17)](https://github.com/BZcreativ/bzdium/releases/tag/v0.2.17) (mirrored on [Forgejo](https://forgejo.bzlab.duckdns.org/buzman/bzdium/releases/tag/v0.2.17)):

- **Debian / Ubuntu / Mint (.deb)** — [`bzdium_0.2.17_amd64.deb`](https://github.com/BZcreativ/bzdium/releases/download/v0.2.17/bzdium_0.2.17_amd64.deb) (5.5 MB):
  ```bash
  sudo apt install ./bzdium_0.2.17_amd64.deb
  # or: sudo dpkg -i bzdium_0.2.17_amd64.deb
  ```
- **Fedora / RHEL / openSUSE (.rpm)** — [`bzdium-0.2.17-1.x86_64.rpm`](https://github.com/BZcreativ/bzdium/releases/download/v0.2.17/bzdium-0.2.17-1.x86_64.rpm) (5.6 MB):
  ```bash
  sudo dnf install ./bzdium-0.2.17-1.x86_64.rpm
  # or: sudo rpm -i bzdium-0.2.17-1.x86_64.rpm
  ```
- **Universal AppImage** — [`bzdium_0.2.17_amd64.AppImage`](https://github.com/BZcreativ/bzdium/releases/download/v0.2.17/bzdium_0.2.17_amd64.AppImage) (85 MB):
  ```bash
  chmod +x bzdium_0.2.17_amd64.AppImage && ./bzdium_0.2.17_amd64.AppImage
  ```
- **Arch Linux / Omarchy / Manjaro (.pkg.tar.zst)** — [`bzdium-bin-0.2.7-1-x86_64.pkg.tar.zst`](https://github.com/BZcreativ/bzdium/releases/download/v0.2.7/bzdium-bin-0.2.7-1-x86_64.pkg.tar.zst) (4.2 MB, last built for v0.2.7 — no newer package has been published):
  ```bash
  sudo pacman -U bzdium-bin-0.2.7-1-x86_64.pkg.tar.zst
  ```
- **Portable Tarball** — [`bzdium-0.2.17-linux-x64.tar.gz`](https://github.com/BZcreativ/bzdium/releases/download/v0.2.17/bzdium-0.2.17-linux-x64.tar.gz) / [`.tgz`](https://github.com/BZcreativ/bzdium/releases/download/v0.2.17/bzdium-0.2.17-linux-x64.tgz) (5.3 MB):
  Extract anywhere and run `./bzdium`. Stores sessions in `bzdium-data/` next to the binary if writable.

*Requires `webkit2gtk-4.1` (or `webkit2gtk-4.0`), `gtk3`, and `libayatana-appindicator3` (preinstalled on most desktop Linux distributions).*

### Windows 10/11 x64

Download from the [Latest Release](https://github.com/BZcreativ/bzdium/releases/latest):

- **Installer** — [`bzdium_0.2.17_x64-setup.exe`](https://github.com/BZcreativ/bzdium/releases/download/v0.2.17/bzdium_0.2.17_x64-setup.exe) (3.0 MB): Start Menu + desktop shortcuts, uninstaller, per-user or all-users. Installed copies keep their data in `%APPDATA%\bzdium` (the install folder is read-only).
- **Portable** — [`bzdium-v0.2.17-x64-portable.exe`](https://github.com/BZcreativ/bzdium/releases/download/v0.2.17/bzdium-v0.2.17-x64-portable.exe) (14 MB): rename to `bzdium.exe`, drop it anywhere writable — even a USB stick — and run; data lives in a `bzdium-data` folder next to it.

Both need the WebView2 runtime (preinstalled on Windows 10/11; the installer can fetch it if missing).

## Build

### Prerequisites

- **Linux**: GCC/Clang, `pkg-config`, `libwebkit2gtk-4.1-dev` (or `webkit2gtk-4.1`), `libgtk-3-dev`, `libayatana-appindicator3-dev`.
- **Windows**: MSVC toolchain, WebView2 runtime (preinstalled on Windows 10/11).

```bash
cd src-tauri
cargo build --release        # -> target/release/bzdium (or bzdium.exe)
cargo test                   # 260+ tests incl. IPC/security contracts
```

Build distribution packages:
- **Linux (deb, rpm, AppImage)**: `npx @tauri-apps/cli build --bundles deb,rpm,appimage`
- **Windows (NSIS setup)**: `npx @tauri-apps/cli build` (installer lands in `target/release/bundle/nsis/`)

> Toolchain note: rustc 1.98.0 is pinned via `rust-toolchain.toml` for compiler stability.

## Security & credential storage

Your logins live **only** in each service's isolated browser profile (`bzdium-data/sessions/<id>/` portable, `~/.local/share/bzdium/sessions/` on Linux, `%APPDATA%\bzdium\sessions\` on Windows when installed) — bzdium itself never sees, stores, or transmits passwords or cookies. Concretely:

- **Windows profiles** encrypt cookies at rest with a DPAPI key bound to your Windows user and machine (`os_crypt.encrypted_key`, DPAPI-prefixed, in each profile's `Local State`). A copied `bzdium-data` folder does not decrypt on another machine or under another user.
- **Linux profiles** isolate state per service into independent WebKitGTK user-data contexts, secured by Unix permissions (read/write restricted to your user UID).
- **Installed copies** keep data in `~/.local/share/bzdium` (Linux) or `%APPDATA%\bzdium` (Windows), protected by standard OS user boundaries. Uninstalling removes the app binary while preserving your session data.
- **Portable copies** inherit the ACLs/permissions of wherever you put them. On a USB stick anyone holding the physical drive holds the profile folders — treat portable folders as sensitive.
- **Config export** contains services + settings only — never sessions, cookies, or credentials.
- App-written files contain no secrets: `services.json`/`settings.json` hold names, URLs, and toggles; nothing secret-shaped. Page titles are parsed to an unread count and discarded. The one credential bzdium stores on your behalf — the optional Grok API key — is encrypted with Windows DPAPI (user-scoped, additional entropy) and never rendered in full, logged, or exported.
- **Network:** bzdium itself makes no telemetry/update calls — the only network traffic is the web content you load, plus the Grok assistant's own calls to `api.x.ai` when you use it (https, pinned host, redirects disabled). The IPC surface exposed to web pages is exactly two commands (`report_title` and `report_probe`, both bound to the calling webview, badge counts capped), enforced by Tauri capabilities + a UI Content-Security-Policy.
- Uploaded custom icons are validated by magic bytes (png/jpg/webp/svg ≤256 KB), stored locally, and rendered only in `<img>` contexts where SVG scripts cannot execute.
- Developer mode (DevTools) is off by default and gated server-side.

## Release History

- **v0.2.17** (per-app fonts):
  - **New: per-app font family and size, with live preview.** Settings → **Fonts** picks a text family (from the curated list or a custom name) and a size from 8 to 40 px for one service at a time, with an instant preview and a slider/stepper; **Apply to all apps** copies the choice to every other service, while each app keeps its own otherwise. The override is delivered as an in-page stylesheet applied at document start, so it survives navigation and reloads, and a best-effort pass also scales text that a site fixes in pixels — images, video, icons and code blocks are deliberately left at their intrinsic size.
  - **New: font menu on right-click.** Increase / Decrease / Reset font size and **Font settings…** sit at the top of the service right-click menu, above the existing Reload / Back / Forward / Home / Open in browser entries; text fields keep the native menu so Paste and Undo still work, and an action only ever affects the service whose page was clicked.
  - **New: customizable, conflict-checked shortcuts.** The four font actions default to `Ctrl+=`, `Ctrl+-`, `Ctrl+0` and `Ctrl+Shift+F` and can be rebound per app from Settings. Duplicate bindings are refused with both actions named, bzdium's own accelerators (`Ctrl+R`, `Ctrl+W`, `Ctrl+Q`, `Ctrl+Shift+I/J`, `Ctrl+F`, `F5`) stay reserved, and `Cmd` is accepted and mapped on macOS.
  - **New: per-app persistence.** Everything is stored with the service in `services.json` and travels through config export/import; a `services.json` written before this release opens unchanged with the defaults (16 px, system font stack, default shortcuts).
- **v0.2.16** (recover a broken session):
  - **New: Clear site data.** The service right-click menu can now wipe one service's stored cookies, local storage, IndexedDB, cache and service workers, then reopen it at its configured URL. This is the fix for state that survives every reload — a replayed one-time sign-in handoff, a service worker serving an obsolete bundle, or a site that has decided the stored session is invalid — without deleting the service or digging through DevTools. It asks for confirmation first, because it signs you out of that service; other services and all settings are untouched. Two unit tests cover the profile removal (existing directory, already-missing directory), and the new command is declared in the build script, the capability and the generated permissions.
- **v0.2.15** (a Settings panel that uses the screen):
  - **New: Settings rebuilt for real desktops.** The dialog is now a wide, sectioned panel — up to 800 px wide and 88% of the viewport height, centred, with General/Interface cards, a full-width Performance & diagnostics card, and dedicated Agent Access API and Grok sections. A fixed header and a pinned action bar keep Cancel, Save, Export and Import visible while only the settings body scrolls, so there is no horizontal scrolling, no clipped controls, and no scrolling to reach Save at 1366×768 or above. Every control keeps its id, label, type and behaviour, and `app.js` is byte-for-byte unchanged.
- **v0.2.14** (Grok):
  - **New: Grok, two ways.** **Grok** opens grok.com through the ordinary webview service architecture; **Grok API** is a native assistant that chats with xAI's Grok models over the official API with streaming replies, cancellation, local conversation history and a live model picker. The API key is stored only as a user-scoped Windows DPAPI blob — never rendered in full, logged, or exported — and provider errors are classified (auth/credits/rate-limit/model/network/timeout) so retry is offered only for transient failures. API mode requires Windows; website mode works everywhere. The built-in catalog grows to 62 services, with 24 new tests plus two opt-in live tests against api.x.ai (see `docs/grok-integration.md`).
- **v0.2.13** (side-loadable WebView2 runtimes):
  - **New: hold a runtime the Evergreen updater deletes.** Hours after v0.2.11 pinned WebView2 155, the Evergreen updater removed the 155 folder from its own roots — the pin's fail-safe correctly degraded to the system runtime (156, which wedges Gmail). Microsoft's supported answer for holding a build is the **Fixed Version** distribution; bzdium now accepts a Fixed Version runtime extracted into **`<dataDir>/webview2-runtimes/<version>/`**: such runtimes appear in the Settings "WebView2 runtime" list, pass the pin's path validation, and are never touched by the updater. This is a third allowed root alongside the machine-wide and per-user Evergreen roots; arbitrary folders remain rejected.
- **v0.2.12** (hardening — external code review):
  - Fix-by-fix remediation of an independent code review of the whole codebase @ v0.2.11 (all four High, most Medium/Low findings; 8 new tests).
  - **Agent layer:** the audit redactor can no longer panic after a mutation has committed (UTF-8 boundary fix; redaction also moved before dispatch); an imported `agentAccess` now actually starts/stops the loopback listener (shared `sync_agent_layer`); tool arguments are validated against the published schemas (a typo'd key is rejected instead of silently running a mutation); MCP JSON-RPC batches capped at 20; `agent/` is 0700 and `tokens.json` 0600 on Linux; the stdio bridge refuses non-`bzd_agt_` tokens and control bytes (header injection).
  - **Webviews:** `navigate` refuses disabled services and never paints two service webviews at once; service and link webviews are locked to https-only navigation; the badge first-report race is fixed (the URL registry is primed before the webview exists); manual hibernate keeps the unread badge; the WebView2 runtime pin must be a runtime under a known Evergreen install root.
  - **Storage/UI:** non-UUID service ids from a tampered `services.json` are dropped at load (they feed `sessions/` path joins); `update_service` rejects empty names; the first agent API key is now actually displayed when minted; the confirm dialog's button names its action (Import / Allow / Create) instead of a hard-coded "Remove"; docs corrected (`BZDIUM_AGENT_PORT`, dryRun exceptions, test counts, rust-version).
- **v0.2.11** (WebView2 runtime pin):
  - **New: pin the WebView2 runtime from Settings (Windows).** The Evergreen runtime auto-updates under running apps, and 156.0.4314.x wedges service webviews within minutes (Gmail observed wedging roughly every few minutes of active use, with the watchdog recovering each time until its 2-per-30-minutes budget ran out). Settings → "WebView2 runtime" now lists every installed runtime and pins all webviews to the chosen folder via the WebView2 loader (`WEBVIEW2_BROWSER_EXECUTABLE_FOLDER`), applied before the first webview is created — so a change takes effect on the next app start. Measured: Gmail on 155.0.4283.62 stays stable where 156.0.4314.50 cycles. A pin that points at a folder the updater later removes is ignored at startup (system default) and shown as "(missing)" in Settings rather than breaking the app. Also exposed to agents via `patch_settings` (`webview2RuntimeFolder`, null clears it).
- **v0.2.10** (watchdog):
  - **New: automatic recovery for stuck pages (on by default).** A second, slower variant of the v0.2.8 wedge was observed with Gmail on WebView2 156.0.4314.50 (2026-10-28): the page renders cached content but its sync dies — "Not connected. Connecting in 3:26…" banner, console repeating `Uncaught (in promise) _.Ltb`. The watchdog injects a liveness probe into every service webview (a `no-store` fetch of the page's own origin, reported back over the same gated IPC channel as unread badges) and hard-reloads a webview that has missed 4 consecutive probes (~3 minutes) while the machine is still online. Fresh webviews get a grace period, the OS network is checked before intervening, and interventions are capped at 2 per service per 30 minutes so a genuinely down site is never reload-spammed. Every reload shows a toast. Toggle in Settings → "Auto-recover stuck pages" (`autoRecover`, also exposed to agents via `patch_settings`).
- **v0.2.9** (Agent Access Layer):
  - **New: an opt-in Agent Access API.** The app's existing command surface is now exposed to local AI agents over two transports — MCP (JSON-RPC 2.0) and REST — both projected from a single tool registry, so a REST call, an MCP `tools/call` and a UI click all reach the same Rust function. Off by default; enable it explicitly in Settings.
  - 21 tools, 7 resources and 5 prompts, with an OpenAPI 3.1 document at `/api/v1/agent/openapi.json` for function-calling clients.
  - Loopback-only listener that rejects any request carrying an `Origin` header and never answers CORS preflight, so a web page cannot drive the app through your browser.
  - Scoped bearer keys (read / write / admin) with a per-token rate limit. Tokens and the admin ceiling are re-read on every request, so revoking a key or un-ticking "Allow admin tools" takes effect immediately, without a restart.
  - Mutating tools accept `dryRun: true` to preview an effect without committing it. Destructive operations are admin-only, and `import_config` additionally requires a confirmation token issued by `export_config`.
  - Errors are structured and always carry an actionable `suggestion`.
  - MCP works over Streamable HTTP and over a `--mcp-stdio` bridge for desktop clients such as Claude Desktop and Cursor.
  - Verified end-to-end against the official MCP TypeScript SDK on both transports: handshake, `tools/list`, `tools/call`, `resources/read`, `prompts/get`, and read/write/admin scope enforcement.
- **v0.2.8** (recovery fix):
  - **Reload is now a hard reload.** A service webview can wedge its WebView2 network stack: the page keeps painting but every request hangs, so the page drops into its own offline state — seen with Gmail (`Not connected.` banner, dimmed list with no sender/subject, messages never open, console repeating `Exceeded max retries while waiting for Gbar to load.`). A soft `location.reload()` never completed in that state. Reload now closes the webview, waits for the old browser process to exit, and recreates it, which is the only thing that clears the wedge.
  - Reloading an inactive service no longer creates its webview visible on top of the active one.
  - Linux bundles rebuilt for this release: `.deb`, `.rpm`, `.AppImage` and the portable tarball.
- **v0.2.7** (security remediation release):
  - `ignore TLS errors` can no longer be enabled for built-in public services (WhatsApp, Gmail, Discord, …) — only for self-hosted instances on other hosts; existing configs on public hosts are silently downgraded on edit/import. A ⚠ flag on the sidebar marks services with the bypass on.
  - Badge updates from page titles are capped (512-byte titles dropped) and rate-limited to one fan-out per second per service.
  - In-app link windows are capped at 5 and are now hidden/restored together with the main window on tray minimize/restore.
  - External-open guard hardened: URLs are parsed (not prefix-matched) and any control byte/NUL/non-http(s) scheme is refused before the OS handler runs.
  - Config import refuses files over 2 MiB; icon picker validates size on the bytes actually read (closes stat-then-read race).
- **v0.2.6**:
  - Fixed badges silently breaking on services served from non-default HTTPS ports (e.g. `:8443`) — the Tauri capability pattern now covers all HTTPS ports.
  - `ignore TLS errors` now actually works on Windows (previously stored but only honored on Linux); scoped per service, verified against a self-signed endpoint.
  - Badge poller no longer runs inside iframes: kills the recurring Tauri IPC 500 errors in the console and stops the `__TAURI__` bridge from leaking into third-party frames (OAuth, reCAPTCHA).
  - Linux packages rebuilt for this release: `.deb`, `.rpm`, and `.AppImage` (glibc, built on Ubuntu 24.04).
- **v0.2.5**:
  - Added per-service "Ignore TLS certificate errors" option (with disclaimer) for self-signed/private-CA endpoints, honored on Linux.
- **v0.2.4**:
  - Added Linux x64 support and official bundles: `.deb`, `.rpm`, `.tar.gz`/`.tgz`, `.AppImage`, and Arch `.pkg.tar.zst`.
  - Fixed external link routing: service links now open via system default browser (`xdg-open` on Linux, `ShellExecuteW` on Windows).
  - Added Home button (`⌂`) and context menu "Open in browser".
  - Configurable link target (`settings.linkTarget`: default browser or isolated in-app window).
  - Linux XDG user data directory fallback (`~/.local/share/bzdium`).
- **v0.2.3**:
  - Security hardening: origin-gated badge reporting (`webview_on_home_origin`), lock-free URL tracking.
  - Closed devtools bypass and restricted capabilities.
  - Pinned rustc 1.98.0 toolchain for stable MSVC compilation.
- **v0.2.2**:
  - Rebranding with custom icons and visual monograms.
  - Built-in icons for 60 services and custom icon image sniffing/validation (PNG/JPEG/WEBP/SVG).
  - Official NSIS Windows installer alongside portable executable.
- **v0.2.1**:
  - Initial public portable Windows x64 release.
  - 15-second product demo video walkthrough.
- **v0.2.0**:
  - Expanded built-in service catalog to 60 services across messaging, email, social, AI, and productivity.
  - Added URL bar with live URL display and visibility toggle.
  - Workspace configuration export & import with atomic JSON writes.
- **v0.1.0**:
  - Proof of concept: from-scratch Rust + Tauri multi-webview architecture replacing Ferdium's heavy Electron runtime.
  - Isolated per-service session cookies, unread badge counter, and RAM hibernation task.

## Debugging the UI

Debug builds auto-open devtools on the UI webview. To script the UI webview over CDP:

```bash
WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222" ./bzdium.exe
# then open http://127.0.0.1:9222/json/list
```

## Troubleshooting

- **A service page shows its own "offline" state and never recovers.** Seen with
  Gmail: the message list dims, rows keep only their category chip and date, the
  header shows "Not connected.", opening a message does nothing, and the console
  repeats `Exceeded max retries while waiting for Gbar to load.`. The webview's
  network stack has wedged — every request from that page hangs, so the page
  cannot even complete a reload of itself. Right-click the service in the sidebar
  and choose **Reload**: it closes and recreates the webview (a fresh WebView2
  browser process), which clears the state. On builds whose Reload only called
  `location.reload()`, use **Hibernate** and then click the service to wake it.

## Layout

- `src-tauri/src/` — Rust backend (state, storage, services, webviews, hibernation, badges, tray, export/import)
- `dist/` — UI (plain HTML/CSS/JS, no build step, no dependencies)
- `docs/superpowers/specs/2026-10-08-bzdium-design.md` — design + **normative** IPC, threading/locking, ACL and geometry contracts
- `docs/code-review-2026-10.md` — the 2026-10 security review and how each finding was resolved

Read the spec before changing IPC, threading, or capabilities — several
contracts there exist to prevent app-wide deadlocks (state lock vs WebView2
nested pump), to keep remote web content from reaching anything but
`report_title`, and to stop imported config files from planting path-traversal
service ids.

## Credits

- [Ferdium](https://github.com/ferdium/ferdium-app) and its ancestors
  ([Ferdi](https://github.com/getferdi/ferdi), [Franz](https://meetfranz.com)) — the concept
- [Tauri](https://v2.tauri.app) + [wry](https://github.com/tauri-apps/wry) — the app framework
- Microsoft WebView2 & WebKitGTK — the rendering engines
