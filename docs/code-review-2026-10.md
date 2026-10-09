# bzdium — Code Review Findings

Scope: full static review of `src-tauri/src/*.rs`, `build.rs`, `capabilities/*.json`,
`tauri.conf.json`, `Cargo.toml`, `dist/*`, and the design spec
(`docs/superpowers/specs/2026-10-08-bzdium-design.md`).
Method: line-by-line reading; Tauri v2 permission surface verified against the
generated ACL artifacts under `src-tauri/target/debug/build/tauri-*/out/permissions/`
and `src-tauri/gen/schemas/capabilities.json`. No files were modified by this review.

Overall: this is a well-structured, well-commented small codebase. The threading
contract is documented and — in the commands — actually respected. The findings
below are mostly about the **remote IPC surface**, **input validation on the import
path**, and **partial-failure states**. Nothing here is exotic; most fixes are a
few lines each.

---

## Findings by severity

| # | Severity | Area | Finding |
|---|---|---|---|
| 1 | High | Remote IPC | Any https page can set the badge of **any** service (`report_title` does not bind `id` to the caller) |
| 2 | High | Availability | Badge count is unbounded → `total_badge_count` overflow (panic in debug, wrap in release) |
| 3 | High | Locking | `report_title` and the hibernation task hold the state mutex across `emit` / `set_title` — violates the project's own locking contract |
| 4 | High | Path traversal | `import_config` preserves arbitrary service `id` strings, which are used to build filesystem paths and webview labels |
| 5 | Medium | Input validation | `import_config` applies imported `settings` verbatim, silently writing the HKCU `Run` autostart key |
| 6 | Medium | Correctness | `enabled` is stored, edited, exported and imported — but never read anywhere. The checkbox is a no-op |
| 7 | Medium | Availability | No throttle on `report_title`; no navigation containment (`on_navigation`) for service webviews |
| 8 | Medium | Consistency | Several commands leave state and reality divergent on partial failure |
| 9 | Medium | Resource leak | Session folders are kept forever on removal/import, with no cleanup path |
| 10 | Medium | UX/perf | Full sidebar rebuild on every `state-changed` event; drag-and-drop can be destroyed mid-drag |
| 11 | Low | Latent bug | `splice(-1, 1)` in the drop handler silently reorders the wrong item |
| 12 | Low | Spec drift | WebView2-missing error is a toast, not the "blocking dialog" the spec mandates |
| 13 | Low | Repo hygiene | Live WebView2 session profiles (real messenger cookies) sit in the working tree |
| 14 | Low | Hardening | No CSP; `withGlobalTauri: true`; duplicate `winreg` in the lockfile |
| 15 | Info | Dead code | `get_settings` is registered, ACL-granted and never called |

---

## 1. High — `report_title` does not bind the reported `id` to the calling webview

**Where:** [badges.rs:57-71](src-tauri/src/badges.rs#L57),
[service-webviews.json:8-11](src-tauri/capabilities/service-webviews.json#L8)

The remote capability grants `allow-report-title` to `webviews: ["service-*"]` at
`remote.urls: ["https://*"]`. The command then trusts the `id` argument blindly:

```rust
match inner.service_mut(&id) {
    Some(service) if service.badge_count != count => { service.badge_count = count; }
    ...
}
```

Nothing ties `id` to the webview that invoked the command. So **any** https page
loaded in **any** service webview can set the badge of **every other** service —
including services it has nothing to do with.

This matters more than it first appears because navigation is unrestricted (see
finding 7): a single malicious link inside any hosted service walks that webview to
`https://evil.example`, and that origin now holds `allow-report-title` for the whole
app. The practical impact is UI spoofing — fake unread counts on a trusted tab, a
doctored window title (`bzdium (N)`), and the overflow in finding 2.

**Fix.** Tauri 2.12 does not hand a command handler the identity of the calling
webview, so the honest mitigation is layered:

1. **Contain navigation** — register `.on_navigation(|url| allowed_origin(service_url, url))`
   on each service webview so a service tab cannot leave its registered origin.
   This is the single change that closes most of the gap.
2. **Bound the value** — clamp `count` (e.g. `min(count, 9999)`) so a remote page
   cannot drive state to extremes.
3. **Throttle** — rate-limit `report_title` per service id.
4. If you want true attribution, inject a per-webview nonce into
   `init_script` and require it in the payload, rejecting reports whose nonce does
   not match the webview that created it.

---

## 2. High — Unbounded badge count → overflow in `total_badge_count`

**Where:** [badges.rs:38-43](src-tauri/src/badges.rs#L38), [state.rs:94](src-tauri/src/state.rs#L94)

Parsing saturates rather than bounds:

```rust
value = value.saturating_mul(10).saturating_add((bytes[j] - b'0') as u32);
```

A page titled `(99999999999)` therefore yields `u32::MAX` (4294967295). Then:

```rust
let total_badge_count = services.iter().map(|s| s.badge_count).sum();
```

`Iterator::sum` for `u32` uses plain `add`, which **panics on overflow in debug
builds** and **wraps silently in release**. So one malicious page reporting a huge
title, plus any second service with a nonzero badge, gives you either a panic in a
dev run or a total that wraps to a nonsense value in the shipped build. The spec's
stated goal is "the app must never panic on user action" — this path breaks it.

**Fix.** Clamp at the parse boundary (`badges.rs`) *and* sum with `saturating_add`:

```rust
// badges.rs
return value.min(MAX_BADGE_COUNT);          // e.g. 9_999

// state.rs
let total_badge_count = services.iter()
    .fold(0u32, |acc, s| acc.saturating_add(s.badge_count));
```

Add a test for `parse_badge_count("(99999999999999) X")`.

---

## 3. High — State mutex held across `emit` / `set_title`

**Where:** [badges.rs:65-82](src-tauri/src/badges.rs#L65), [hibernate.rs:73-78](src-tauri/src/hibernate.rs#L73)

The spec's normative rule is explicit:

> The state mutex is NEVER held across webview operations … then emit
> `state-changed` from the pre-built snapshot (`emit_snapshot`).

`report_title` is a **sync** command (so it runs on the event-loop thread) and it
holds the guard across both a window operation and an emit:

```rust
let mut inner = state.lock()...;      // guard acquired
...
let total = inner.snapshot().total_badge_count;
let _ = window.set_title(&title);     // window op, guard still held
emit_state_changed(&app, &inner);     // emit -> IPC eval into a webview, guard still held
```

`emit_state_changed` is the *lock-holding* variant; the codebase already provides
`emit_snapshot` precisely so callers can drop the guard first — and every other
module uses it correctly. `hibernate.rs` phase 3 repeats the same mistake:

```rust
if let Ok(inner) = state.lock() {
    emit_state_changed(&app, &inner);   // guard alive during the emit
};
```

Whether this deadlocks depends on Wry internals (does `emit_to` → `eval` pump a
nested message loop?). But the failure mode is exactly the one the spec says was
*observed in this app*: a nested pump processes another sync command
(`get_state`, a second `report_title`), which blocks on the mutex the outer call
still owns. The fix is free and removes the question entirely.

**Fix.** Build the snapshot, drop the guard, then act:

```rust
let (total, snapshot) = {
    let mut inner = state.lock().map_err(|e| e.to_string())?;
    match inner.service_mut(&id) {
        Some(s) if s.badge_count != count => s.badge_count = count,
        _ => return Ok(()),
    }
    (inner.snapshot().total_badge_count, inner.snapshot())
};   // guard dropped here
// ... set_title ...
emit_snapshot(&app, &snapshot);
```

Same shape for `hibernate.rs` phase 3.

---

## 4. High — `import_config` preserves arbitrary service ids, which become paths

**Where:** [services.rs:394-403](src-tauri/src/services.rs#L394),
[webviews.rs:51-54](src-tauri/src/webviews.rs#L51)

```rust
id: if s.id.trim().is_empty() { Uuid::new_v4().to_string() } else { s.id.clone() },
```

Ids are kept verbatim "so existing local sessions reconnect" — reasonable intent,
but the id is later used to build a filesystem path and a webview label:

```rust
let session_dir = data_dir.join(SESSIONS_DIR).join(&service.id);
fs::create_dir_all(&session_dir)...
```

`Path::join` performs no normalization, so an id of `..\..\..\Users\me\AppData`
escapes the data directory and `create_dir_all` creates it. Two further consequences
of an unvalidated id:

- **Duplicate ids** are not deduplicated. Two services sharing an id share one
  WebView2 profile (cookie cross-contamination between, say, a personal and a
  work account) and one webview label — the second `create_service_webview` becomes
  a silent no-op, so both sidebar tabs drive the same page.
- `label_for(id)` embeds the raw id into `service-<id>`, and `show_only` slices it
  back out. The slicing itself is safe (the `"service-"` prefix is ASCII, so byte
  index 8 is always a char boundary), but odd ids produce odd labels.

**Fix.** Validate ids on import, and dedupe:

```rust
fn normalize_imported_id(raw: &str, seen: &mut HashSet<String>) -> String {
    let id = raw.trim();
    let ok = id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok || !seen.insert(id.to_string()) {
        return Uuid::new_v4().to_string();
    }
    id.to_string()
}
```

Rejecting `.`/`..`/separators outright is the important part.

---

## 5. Medium — Imported settings silently enable autostart

**Where:** [services.rs:408-423](src-tauri/src/services.rs#L408)

`inner.settings = config.settings;` takes the imported struct wholesale, then
`crate::autostart::apply(start_with_windows)` writes
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` pointing at the current exe.
A crafted config file therefore flips a persistence mechanism with no confirmation
beyond the generic "this REPLACES all services and settings" prompt. There is also
no size cap on the file read (`std::fs::read_to_string`) and no cap on
`hibernationMinutes` (a `u64` accepted unbounded).

**Fix.** Confirm the autostart change explicitly after import, clamp
`hibernation_minutes` to a sane range (the UI already caps at 1440 — enforce it
server-side too), and reject absurdly large config files before parsing.

---

## 6. Medium — `enabled` is a dead field

**Where:** written at [services.rs:193](src-tauri/src/services.rs#L193); read nowhere.

A content search for `.enabled` across `src-tauri/src` returns only the write, the
export mapping, and the import mapping. Nothing in `activate`,
`create_service_webview`, `show_only`, `hibernate.rs`, or `snapshot()` consults it.
The UI never filters or dims a disabled service either — `render()` iterates
`state.services` unconditionally.

So the "Enabled" checkbox in the Edit dialog accepts a value, persists it, exports
it, and has no effect. That is a user-visible broken feature, not just unused code.

**Fix.** Pick one: honour it (skip disabled services in activation, hibernation and
rendering; show them dimmed), or remove the field from `Service`, the edit form,
and `ConfigService`. Leaving it is the worst option.

---

## 7. Medium — No throttle on `report_title`, no navigation containment

**Where:** [badges.rs:57](src-tauri/src/badges.rs#L57), [webviews.rs:57-70](src-tauri/src/webviews.rs#L57)

The injected script polls every 3 s and reports on *any* title change. A page that
mutates `document.title` aggressively (many sites do, for typing indicators) fires a
sync command on the event-loop thread each time, each taking the global mutex. When
the parsed count alternates, each also serializes and emits a full `AppState`
snapshot to the UI. With dozens of services this is a self-inflicted UI amplifier,
reachable from web content.

Separately, `WebviewBuilder` is created with no `on_navigation` hook, so a service
webview can be walked to any URL by page content. That is what turns finding 1 from
theoretical into reachable.

**Fix.** Add `on_navigation` restricted to the service's registered origin (with an
explicit allowlist for legitimate cross-domain auth flows), and rate-limit
`report_title` per id.

---

## 8. Medium — Partial failures leave state and reality divergent

Each of these mutates and **persists** state under the lock, then performs the
webview operation unlocked — correct for deadlock safety, but the error is returned
after the durable state already changed.

| Path | What persists | What fails | What the user sees |
|---|---|---|---|
| [hibernate_service:353-368](src-tauri/src/webviews.rs#L353) | `hibernated = true`, saved | `close_webview` | Flag says hibernated; the page is still live and polling |
| [update_service:183-210](src-tauri/src/services.rs#L183) | new URL, saved | webview recreation | Saved URL is new; the visible page is still the old one |
| [activate:168-188](src-tauri/src/webviews.rs#L168) | `active_service_id`, saved | webview creation | Tab marked active with a blank content area |
| [import_config:408-434](src-tauri/src/services.rs#L408) | all services + settings | `activate(...)?` returns early | All webviews closed, state replaced, **no `state-changed` emitted** → UI shows the pre-import list |

`import_config` is the worst of these: the `?` at line 430 aborts before
`emit_snapshot`, so the UI is left rendering a state that no longer exists.

**Fix.** On webview-operation failure, roll back the in-memory flag and re-save, or
at minimum always emit the snapshot before returning `Err` so the UI cannot drift.
For `import_config`, close over the activation error into the outcome rather than
propagating it.

Related: `hibernate_service` never clears `active_service_id` and never calls
`show_only`, so hibernating the *active* service leaves the sidebar showing an
active tab over an empty content area.

---

## 9. Medium — Session folders accumulate without bound

**Where:** [services.rs:228](src-tauri/src/services.rs#L228)

Keeping the session on remove is a deliberate spec decision, and it is defensible.
But a WebView2 user-data folder is routinely tens to hundreds of MB, and nothing
ever reclaims one — not removal, not import (which orphans ids that disappear from
the new config), not any UI action. A user who churns services grows `bzdium-data`
monotonically, in a folder the product markets as "portable" and users copy around.

**Fix.** Offer an explicit "delete stored session" action in the remove-confirm
dialog (default off), and on import, report ids whose session folders are now
unreferenced.

---

## 10. Medium — Full sidebar rebuild on every event

**Where:** [app.js:143-146](dist/app.js#L143)

`render()` does `serviceList.textContent = ""` and rebuilds every item, listener and
drag handler. It runs on *every* `state-changed` — including every badge change from
every service. Two consequences:

- With many services and chatty titles, this is constant DOM churn on the UI thread.
- A badge event arriving mid-drag destroys the element being dragged, so the drop
  never fires and the reorder silently fails.

**Fix.** Diff by service id and update only what changed (badge text, active class,
hibernated class), or at minimum suppress re-render while `draggedId !== null`.

---

## 11. Low — `splice(-1, 1)` in the drop handler

**Where:** [app.js:292-297](dist/app.js#L292)

```js
orderedIds.splice(orderedIds.indexOf(draggedId), 1);
```

If `draggedId` is not in `orderedIds`, `indexOf` returns `-1` and `splice(-1, 1)`
removes the **last** element — then the dragged id is inserted, so the user's drop
reorders a service they never touched. Reachable when a `state-changed` event
rebuilds the list between `dragstart` and `drop` (see finding 10) while the dragged
service has gone away.

**Fix.** Guard the index:

```js
var from = orderedIds.indexOf(draggedId);
if (from === -1) { draggedId = null; return; }
orderedIds.splice(from, 1);
```

---

## 12. Low — Spec/implementation drift

- **WebView2 missing.** Spec §3: "show a blocking dialog with the WebView2 download
  link." Implementation: `webview_error()` returns a string that surfaces as a
  transient red toast ([webviews.rs:22-26](src-tauri/src/webviews.rs#L22)). The
  download link is in the text, but toasts auto-dismiss after 4 s — for the one
  error class that blocks all usage, that is the wrong affordance.
- **Recipe list.** Spec §2 lists Signal; the normative §6 list and the code omit it.
  The test pins §6, so §2 is the stale one.
- **`exported_at`.** `chrono_like_now()` emits a bare unix-seconds string
  ([services.rs:304-309](src-tauri/src/services.rs#L304)) under a field name that
  reads like an ISO timestamp. Either name it `exportedAtUnix` or emit RFC 3339.

---

## 13. Low — Repo hygiene: live credentials in the working tree

`bzdium-portable/bzdium-data/sessions/` contains four complete WebView2 profiles —
real cookies and session storage for whatever messengers were used during testing.
`.gitignore` covers `bzdium-portable/`, so they are not committed, but they are on
disk in a folder people routinely zip and share, and the whole point of the portable
layout is that this folder travels.

Also worth noting: `src-tauri/target/` holds ~1,400 build artifacts and is correctly
ignored, but it made the tree expensive to search — worth a `git clean -xdf`
discipline note in the README.

**Fix.** Add a "clear all sessions" action, and consider a warning in the README
that `bzdium-data/sessions/` contains live login state before it is copied anywhere.

---

## 14. Low — Hardening gaps

- **No CSP.** `tauri.conf.json` has `"security": {}`
  ([tauri.conf.json:21](src-tauri/tauri.conf.json#L21)), so no CSP is injected into
  the UI webview. The UI is static local content and builds all DOM via
  `textContent`/`createElement` — I found no `innerHTML`, `insertAdjacentHTML`,
  `eval`, or `new Function` anywhere in `dist/`, which is genuinely good. A CSP is
  still cheap defence-in-depth for the chrome webview.
- **`withGlobalTauri: true`** exposes `window.__TAURI__` to every webview, including
  remote ones. The ACL correctly limits remote pages to `allow-report-title`
  (verified in `gen/schemas/capabilities.json`), so this is not a hole today — but it
  means any future permission added to the remote capability is immediately reachable
  from web content. Keep that capability minimal.
- **`core:default` is broad.** Verified from the generated manifest: it expands to
  `core:path`, `core:event`, `core:window`, `core:webview`, `core:app`, `core:image`,
  `core:resources`, `core:menu`, `core:tray`. That is granted to the *local* UI
  webview only, which is acceptable, but `core:webview:default` includes
  `allow-get-all-webviews` and `allow-internal-toggle-devtools`. If you do not use
  them, enumerate the specific permissions you need instead of the umbrella.
- **Duplicate `winreg`** (0.55.0 and 0.56.0) in `Cargo.lock`
  ([Cargo.lock:4759, 4769](src-tauri/Cargo.lock#L4759)) — one direct, one transitive.
  Aligning them trims the tree.

---

## 15. Info — Dead API

`get_settings` is registered in `lib.rs`, declared in `build.rs`, granted in
`default.json`, and never invoked from `dist/app.js` (the UI reads settings from the
`AppState` snapshot). Harmless, but it is surface area with no caller.

---

## What this codebase does well

Worth stating explicitly, because these are the things a reviewer usually has to ask
for:

- **Lock discipline is documented and mostly honoured.** The "never hold the mutex
  across a webview operation" rule is stated in the spec, restated in code comments
  at each site, and implemented correctly in `activate`, `add_service`,
  `update_service`, `remove_service`, `reorder_services`, `set_overlay_mode`,
  `update_settings`, and hibernation phases 1-2. Findings 3 is the exception, not
  the rule.
- **The frontend never injects HTML.** Every DOM node is built with
  `createElement` + `textContent`. Service names, URLs and toast text — all
  attacker-influenced — never reach an HTML parser. That is the right instinct and
  it holds consistently.
- **Crash-safe storage.** Atomic write-temp-fsync-rename, and corrupt JSON is
  preserved as `*.broken-<ts>` rather than discarded or fatal. `Settings` backfills
  new fields via `serde(default)` so old files keep working.
- **The remote capability is genuinely minimal** — one command, one pattern — and
  there is a test (`tests/remote_pattern.rs`) that replicates Tauri's own
  `RemoteUrlPattern::from_str` pipeline to pin `https://*` behaviour instead of
  trusting it. That is unusually careful.
- **Tests target the risky logic.** Badge parsing has good edge-case coverage
  (`(12 new) (3) X` → 3, `()` → 0, bullet-vs-number precedence).
- **The spec is normative and the code cites it by section.** Several comments point
  at "spec section 6", which makes drift detectable.

---

## Suggested fix order

1. Findings **2 + 3** together — clamp badge parsing, `saturating_add` the total, and
   switch `badges.rs` / `hibernate.rs` to `emit_snapshot` after dropping the guard.
   Small, mechanical, removes both a panic and the deadlock question.
2. Finding **4** — validate and dedupe imported service ids. This is the only finding
   with a filesystem-escape consequence.
3. Finding **7** — add `on_navigation` origin containment. This is what makes
   findings 1 and 2 reachable in practice.
4. Finding **1** — clamp + throttle `report_title`, and decide whether a per-webview
   nonce is worth the complexity.
5. Finding **6** — make `enabled` real or delete it.
6. Finding **8** — always emit the snapshot before returning `Err`; fix the
   `import_config` early return.
7. Findings **9-13** as a hygiene pass.
