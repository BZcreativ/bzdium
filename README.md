# bzdium

**A lightweight, portable multi-messenger for Windows x64 — the [Ferdium](https://github.com/ferdium/ferdium-app) concept, re-implemented from scratch in Rust.**

One window, all your messengers: WhatsApp, Telegram, Discord, Slack, Gmail and 50+ more as real, isolated web apps — with unread badges, hibernation for low RAM, and a single ~12 MB portable exe. No Electron, no bundled Chromium: bzdium uses the system WebView2 runtime.

> **Relationship to Ferdium:** bzdium is an independent, from-scratch re-implementation of the Ferdium *concept* (one window, many messaging services, per-service session isolation, unread badges, hibernation). It contains **no Ferdium code** — the original is Electron/JavaScript and licensed AGPL-3.0; bzdium is Rust + [Tauri](https://v2.tauri.app) + vanilla JS. All credit for the original idea and UX goes to the Ferdium team and its Ferdi/Franz ancestors.

## Why

Ferdium is a great idea in a heavy package: full Chromium per app, hundreds of MB of RAM at idle, and for this author, crashes. bzdium keeps the idea and drops the weight:

| | Ferdium (Electron) | bzdium (Rust/WebView2) |
|---|---|---|
| Bundle | ~100 MB installer, bundled Chromium | single **~12 MB** exe, system WebView2 |
| Idle RAM | 300–600+ MB | **~30 MB** |
| Per-service isolation | Chromium partitions | real WebView2 profiles under `bzdium-data/sessions/` |
| Recipes | 700+ (per-service JS hacks) | 60 popular web clients + any custom https URL |
| Crash safety | — | atomic JSON writes, corrupt-file quarantine |

Measured on Windows 11, 2 live messenger webviews.

## Features

- **Single portable `bzdium.exe`** — data lives in `bzdium-data\` next to it (falls back to `%APPDATA%\bzdium` on read-only media)
- **60 built-in services** (messaging, mail, social, AI, productivity) + custom URLs for anything self-hosted
- **Persistent isolated sessions** — each service gets its own WebView2 profile; logins survive restarts
- **Unread badges** parsed from page titles (capped, spoof-resistant: a page can only ever report its own count)
- **Hibernation** — idle services' webviews are closed after N minutes and wake on click, freeing RAM
- **URL bar** with live URL display and a visibility toggle
- **Workspace export / import** — settings + services as a JSON file (service ids preserved, so local sessions reconnect)
- Dark/light UI, system tray + minimize-to-tray, start-with-Windows, drag-to-reorder, single-instance

## Build

Requires the MSVC toolchain and the WebView2 runtime (preinstalled on Windows 10/11).

```
cd src-tauri
cargo build --release        # -> target/release/bzdium.exe
cargo test                   # 22 tests incl. IPC/security contracts
```

Copy `bzdium.exe` anywhere writable and run it.

> Toolchain note (this machine): rustc 1.98/1.99 intermittently crashes inside
> the compiler itself (memory corruption signature). Builds are incremental —
> re-run `cargo build --release` until it finishes. `src-tauri/.cargo/config.toml`
> raises `RUST_MIN_STACK`, fixing the deterministic stack-overflow variant.

## Debugging the UI

Debug builds auto-open devtools on the UI webview. To script the UI webview over CDP:

```
WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222" ./bzdium.exe
# then open http://127.0.0.1:9222/json/list
```

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
- Microsoft WebView2 — the rendering engine
