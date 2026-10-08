# bzdium

A light, portable multi-messenger for Windows x64 — a Rust/WebView2 re-implementation of the [Ferdium](https://github.com/ferdium/ferdium-app) concept. One window, many messaging services, unread badges, hibernation for low RAM. No Electron, no bundled Chromium: it uses the system WebView2 runtime.

- Single portable `bzdium.exe` (~12 MB) + `bzdium-data/` folder next to it
- Each service gets a real isolated WebView2 profile (cookies/sessions survive restarts)
- Idle services hibernate (webview closed, memory freed) and wake on click
- Unread badges parsed from page titles; dark UI; tray icon; minimize to tray

## Build

Requires the MSVC toolchain and WebView2 runtime (preinstalled on Windows 10/11).

```
cd src-tauri
cargo build --release        # → target/release/bzdium.exe
cargo test                   # unit + IPC contract tests
```

Copy `bzdium.exe` anywhere writable; data lives in `bzdium-data/` next to it
(falls back to `%APPDATA%/bzdium` on read-only media).

Known toolchain issue on this machine: rustc crashes intermittently
(STATUS_ACCESS_VIOLATION / HEAP_CORRUPTION inside the compiler) — both 1.98 and
1.99. Builds are incremental, so just re-run `cargo build --release` until it
finishes. `src-tauri/.cargo/config.toml` raises `RUST_MIN_STACK` for rustc
threads, which fixes the deterministic stack-overflow variant.

## Debugging the UI

Debug builds auto-open devtools on the UI webview. To script the UI webview:

```
WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222" ./bzdium.exe
# then open http://127.0.0.1:9222/json/list (CDP)
```

## Layout

- `src-tauri/src/` — Rust backend (state, storage, services, webviews, hibernation, badges, tray)
- `dist/` — UI (plain HTML/CSS/JS, no build step)
- `docs/superpowers/specs/2026-10-08-bzdium-design.md` — design + normative IPC/threading/security contracts

Read the spec before changing IPC, threading, or capabilities — several
contracts there exist to prevent app-wide deadlocks (state lock vs WebView2
nested pump) and to keep remote pages from reaching anything but
`report_title`.
