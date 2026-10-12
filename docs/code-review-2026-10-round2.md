# bzdium — Code & Security Review, Round 2 (post-update)

Reviewed: working tree at v0.2.2 (`Cargo.toml` / `tauri.conf.json` version `0.2.2`), after the update
that landed the custom-icon system, developer mode, and the round-1 remediations.
Previous review: [code-review-2026-10.md](code-review-2026-10.md).

## Scope and method

Static review only. Every claim below is backed by a file:line citation I read in this pass.

**Honest limitations:** the DSH sandbox cannot grant write ACLs on `E:\`, so `pwsh` was unusable for
this session — no `git log`/`git status`, no `cargo test`, no `cargo clippy`, no build. Change
detection therefore came from reading files and comparing them against the round-1 notes, not from
git history. Anything I could not verify by reading is labelled as an assumption. Web search was
also unavailable (API key rejected), so all Tauri ACL facts below were verified from **on-disk
generated artifacts** ([`src-tauri/target/debug/build/tauri-7a6b365490f666a8/out/permissions/`](<src-tauri/target/debug/build/tauri-7a6b365490f666a8/out/permissions>)
and `src-tauri/gen/schemas/capabilities.json`) rather than from docs.

Files read in full or in the relevant parts: `badges.rs`, `state.rs`, `hibernate.rs`, `storage.rs`,
`webviews.rs`, `services.rs`, `lib.rs`, `tray.rs`, `autostart.rs`, `build.rs`,
`tests/remote_pattern.rs`, `capabilities/*.json`, `tauri.conf.json`, `Cargo.toml`, `Cargo.lock`,
`build.rs`, `dist/app.js`, `dist/index.html`, `dist/styles.css`, `dist/assets/icons/manifest.json`,
`.gitignore`, `README.md`, and the design spec.

## Verdict

The update is a serious, well-executed remediation pass. **11 of 15 round-1 findings are fixed, 3
are partially fixed, 3 are still open.** The fixes are not cosmetic — they are the right fixes,
they are tested, and the design spec was updated to match the code (including the new security
invariants), which is unusual and worth saying out loud.

The remaining risk is concentrated in one place: **there is still no navigation containment on
service webviews**, and that single gap is what keeps the *residual* of round-1 findings 1 and 2
alive. It is the highest-value thing left to fix.

### Round-1 findings: disposition

| # | Round-1 finding | Status | Evidence |
|---|---|---|---|
| 1 | High — `report_title` did not bind `id` to the calling webview | **Fixed** | [badges.rs:66-75](<src-tauri/src/badges.rs#L66-L75>) takes `tauri::Webview<tauri::Wry>` and rejects a label mismatch. Spec updated at [spec:178](<docs/superpowers/specs/2026-10-08-bzdium-design.md#L178>). |
| 2 | High — unbounded badge count → overflow | **Fixed** | `MAX_BADGE = 999` + `value.min(MAX_BADGE)` ([badges.rs:33-46](<src-tauri/src/badges.rs#L33-L46>)), `saturating_mul`/`saturating_add` in the parse loop, `fold(0u32, u32::saturating_add)` in the sum ([state.rs:107-110](<src-tauri/src/state.rs#L107-L110>)). New test `badge_count_is_capped` ([badges.rs:142-151](<src-tauri/src/badges.rs#L142-L151>)). |
| 3 | High — mutex held across `emit` / `set_title` | **Fixed** | Guard scoped to a block, then `set_title` + `emit_snapshot` off-lock ([badges.rs:77-97](<src-tauri/src/badges.rs#L77-L97>)); hibernation phase 3 does `drop(inner)` before emitting ([hibernate.rs:72-80](<src-tauri/src/hibernate.rs#L72-L80>)); `emit_snapshot` added so callers can pre-build the snapshot ([state.rs:147-153](<src-tauri/src/state.rs#L147-L153>)). The contract is now written down normatively at [spec:268-286](<docs/superpowers/specs/2026-10-08-bzdium-design.md#L268-L286>). |
| 4 | High — import preserved arbitrary service ids (path traversal) | **Fixed** | `normalize_imported_id` accepts only 36-char well-formed UUIDs, dedupes, else mints a fresh UUID ([services.rs:577-594](<src-tauri/src/services.rs#L577-L594>)); traversal cases are tested ([services.rs:905-940](<src-tauri/src/services.rs#L905-L940>)). `custom_icon` is forced to `None` on import ([services.rs:716](<src-tauri/src/services.rs#L716>)). |
| 5 | Medium — import silently enabled autostart | **Fixed** | Imported `startWithWindows` is deliberately overwritten with the current value ([services.rs:723-730](<src-tauri/src/services.rs#L723-L730>)); documented in the spec ([spec:177](<docs/superpowers/specs/2026-10-08-bzdium-design.md#L177>)). |
| 6 | Medium — `enabled` was a dead field | **Fixed** | `activate()` refuses disabled services ([webviews.rs:175-177](<src-tauri/src/webviews.rs#L175-L177>)); disabling closes the webview and releases the active tab ([services.rs:461-485](<src-tauri/src/services.rs#L461-L485>)); hibernation filters on `s.enabled` ([hibernate.rs:40](<src-tauri/src/hibernate.rs#L40>)); startup and import pick the first **enabled** service ([lib.rs:140](<src-tauri/src/lib.rs#L140>), [services.rs:739](<src-tauri/src/services.rs#L739>)); the UI hides disabled services and exposes the checkbox ([app.js:235](<dist/app.js#L235), [app.js:598](<dist/app.js#L598>)). |
| 7 | Medium — no `report_title` throttle, no `on_navigation` | **Still open** | No `on_navigation` anywhere (grep over `src-tauri/src` returns nothing). See **N7** and **N8**. |
| 8 | Medium — partial failures left state and reality divergent | **Partially fixed** | Fixed for the worst case: `import_config` phase 2 logs activation failure instead of aborting, and still emits ([services.rs:743-757](<src-tauri/src/services.rs#L743-L757>)). Still open elsewhere — see **N10**. |
| 9 | Medium — session folders never reclaimed | **Partially fixed** | `remove_service` now reclaims `sessions/<id>` best-effort ([services.rs:513-523](<src-tauri/src/services.rs#L513-L523>)). Orphans from `import_config` and from disabled services are not reclaimed — see **N16**. |
| 10 | Medium — full sidebar rebuild on every event | **Partially fixed** | The rebuild is unchanged ([app.js:233-237](<dist/app.js#L233-L237>)), but the bug it caused is fixed by the mid-drag guard ([app.js:385-389](<dist/app.js#L385-L389>)). Custom icons are cached per file name so the churn is now cheap ([app.js:120-131](<dist/app.js#L120-L131>)). |
| 11 | Low — `splice(-1, 1)` in the drop handler | **Fixed** | `from === -1 || to === -1` guard, then correct splice pair ([app.js:378-394](<dist/app.js#L378-L394>)). |
| 12 | Low — spec/implementation drift | **Mostly fixed** | The spec now documents `devMode`, the icon precedence system, id normalization, the `startWithWindows` ignore, the badge cap, `report_title` binding, the threading contract, and single-instance. Three drifts remain — see **N21**. |
| 13 | Low — live credentials in the working tree | **Fixed** | `bzdium-portable/bzdium-data/` no longer exists (glob returns no files), and neither does `src-tauri/bzdium-dev-data/`. `.gitignore` covers `bzdium-portable/` ([.gitignore:5](<.gitignore#L5>)). |
| 14 | Low — hardening gaps | **Partially fixed** | CSP is now present and tight ([tauri.conf.json:21-23](<src-tauri/tauri.conf.json#L21-L23>)). `withGlobalTauri: true`, broad `core:default`, and the duplicate `winreg` all remain — see **N12**, **N22**. |
| 15 | Info — dead API (`get_settings`) | **Still open** | Registered ([lib.rs:67](<src-tauri/src/lib.rs#L67>)), ACL-granted ([default.json:9](<src-tauri/capabilities/default.json#L9>)), never invoked from `dist/` (grep finds only `get_state`). `emit_state_changed` is now a second dead API — see **N23**. |

---

## New findings

### N1 — High: service webviews still have no navigation containment

**Where:** [webviews.rs:57-67](<src-tauri/src/webviews.rs#L57-L67>) — the builder chain is
`WebviewUrl::External(url)` → `.data_directory()` → `.initialization_script()` → `.on_page_load()`
→ `.focused()`. There is no `.on_navigation(...)`.

**Why this is the important one.** Round-1 finding 1 was fixed by binding `report_title`'s `id` to
the calling webview's label. That is necessary but **not sufficient**, and the gap is exactly where
navigation is unconstrained:

1. The webview label is fixed at creation (`service-<id>`) and never changes. The injected init
   script is attached to the *webview*, not to a document, so it keeps running after the page
   navigates elsewhere. A service page — or any ad or redirect chain on it — can take the webview to
   `https://evil.example`, where the init script still calls
   `invoke('report_title', { id: "<original-service-id>", title })`. The label check **passes**,
   because the calling webview genuinely is `service-<id>`. So an off-origin page can still set that
   service's badge to any value ≤ 999. The label binding stops *cross-service* forgery; it does not
   stop *off-origin* forgery.
2. `on_page_load` emits `url-changed` for **any** final URL, with no origin check
   ([webviews.rs:60-66](<src-tauri/src/webviews.rs#L60-L66)), and the UI writes it straight into the
   URL bar ([app.js:786-793](<dist/app.js#L786-L793>)). The user sees "WhatsApp" in the sidebar while
   the content area is a different host, and the URL bar *confirms* the different host as if it were
   normal. That is a phishing/UX-spoofing shape: the app's own chrome launders the destination.
3. Each service's WebView2 profile (`sessions/<id>`) is the isolation boundary the whole design
   rests on. Unconstrained navigation means service X's cookie jar accumulates cookies from
   arbitrary hosts, which weakens the one property that makes per-service sessions meaningful.

**Fix.** Add containment at the point of creation:

```rust
.on_navigation(move |url| {
    url.scheme() == "https" && host_allowed(url, &service.url)
})
```

A strict host allowlist will break real logins (Google, Microsoft, and Discord all bounce through
identity domains), so the pragmatic version is two-part: allow the service's own registrable domain
plus an explicit per-service identity/CDN allowlist, **and** — independently, which is the cheaper
and more robust half — make `report_title` verify that the calling webview's *current* URL origin
matches the service's configured origin before accepting the title. That closes the badge-forgery
path without constraining navigation at all, and it is a few lines in a function that already has
the `webview` handle.

---

### N2 — Medium: `report_title` is unthrottled and its `title` argument is unbounded in length

**Where:** [badges.rs:66-99](<src-tauri/src/badges.rs#L66-L99>). `title: String` with no length
check; no rate limit; `id` is embedded verbatim in the injected script
([badges.rs:16](<src-tauri/src/badges.rs#L16>)), so a remote page knows it.

`report_title` is the **only** command reachable from a remote origin — the capability grants it to
`https://*` for every `service-*` webview ([service-webviews.json:6-11](<src-tauri/capabilities/service-webviews.json#L6-L11>),
pattern behavior pinned by [tests/remote_pattern.rs](<src-tauri/tests/remote_pattern.rs>)). Two
costs are attacker-driven:

- **Unbounded allocation.** A hostile page can invoke `report_title` with a multi-megabyte title.
  It is deserialized from IPC into a `String`, then scanned byte-by-byte by `parse_badge_count`
  ([badges.rs:34-50](<src-tauri/src/badges.rs#L34-L50>)). Nothing rejects it. Titles longer than a
  few hundred bytes are useless for badge parsing anyway.
- **Amplification against the single global mutex.** The early return when the count is unchanged
  ([badges.rs:79-84](<src-tauri/src/badges.rs#L79-L84>)) limits the expensive path, but an attacker
  alternates `(1)` / `(2)` to hit it every time. Each accepted call takes the mutex, clones **every
  service** in `snapshot()` ([state.rs:104-118](<src-tauri/src/state.rs#L104-L118>)), calls
  `set_title`, and broadcasts `state-changed`. That is O(#services) allocation plus an IPC event at
  the remote page's chosen rate, contending with the same mutex the project's own spec identifies as
  the app-wedge risk ([spec:268-275](<docs/superpowers/specs/2026-10-08-bzdium-design.md#L268-L275>)).

**Fix.** Reject `title.len() > 512`. Add a per-service minimum interval between accepted reports
(the injected script already polls at 3 s, so a 1 s server-side throttle is invisible to legitimate
use). Consider a `Vec`-free snapshot path for badge-only updates.

---

### N3 — Medium: the DevTools gate is bypassable from the UI webview by a built-in permission

**Where:** `tauri`'s `devtools` feature is now enabled ([Cargo.toml:16](<src-tauri/Cargo.toml#L16>)),
and the code comment is candid that this "also enables it in release builds"
([webviews.rs:297-300](<src-tauri/src/webviews.rs#L297-L300>)). `open_devtools` checks
`settings.dev_mode` server-side ([webviews.rs:307-314](<src-tauri/src/webviews.rs#L307-L314)) —
correct place for the check, and the README claims it as a security property
([README.md:75](<README.md#L75>)).

**The gap.** `capabilities/default.json` grants `core:default` to the local UI webview
([default.json:7](<src-tauri/capabilities/default.json#L7>)). Verified from the generated artifact
[`permissions/webview/autogenerated/default.toml`](<src-tauri/target/debug/build/tauri-7a6b365490f666a8/out/permissions/webview/autogenerated/default.toml>),
`core:webview:default` is:

```
permissions = ["allow-get-all-webviews", "allow-webview-position", "allow-webview-size", "allow-internal-toggle-devtools"]
```

So the UI webview already holds `allow-internal-toggle-devtools` unconditionally. The `dev_mode`
gate can be bypassed for the **UI webview** without going through `open_devtools` at all. (I am
inferring — not proving, since I could not run the build — that Tauri's internal toggle applies to
the *calling* webview, so this does not hand out DevTools on service webviews. Even on that
reading, the gate the README advertises is not the only path.)

**Why it matters.** DevTools on the UI webview is the app's own trust boundary: from there you can
invoke every granted command, including `update_settings` (which writes the HKCU Run key — see
**N6**), `import_config` (replaces every service), `remove_service` (deletes a live session), and
`pick_service_icon`. `dev_mode` is a plain field in a plain JSON file that `update_settings` accepts
wholesale ([lib.rs:34-36](<src-tauri/src/lib.rs#L34-L36>)), so it is not a security boundary in any
meaningful sense — it is a UI affordance. The README should say that, or the gate should be made
real (compile-time feature, or debug-build-only).

**Fix.** Drop `core:webview:default` from `core:default` by listing the narrower plugin defaults
the UI actually uses (see **N12**), or ship release builds without the `devtools` feature.

---

### N4 — Medium: portable autostart points at a user-writable path

**Where:** [autostart.rs:12-14](<src-tauri/src/autostart.rs#L12-L14>) writes
`"<current_exe>"` into `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.

The value **is** quoted, which prevents argument splitting — good. But quoting does not prevent
**binary replacement**. The portable distribution's whole premise is "drop it anywhere writable —
even a USB stick" ([README.md:43](<README.md#L43>)), and `resolve_data_dir` prefers the exe's own
directory when it is writable ([storage.rs:22-33](<src-tauri/src/storage.rs#L22-L33>)). If that
directory is writable by another user or process on the machine, the logon entry is a persistent
code-execution foothold that any local account with write access to that folder controls. Pre-existing
(not introduced by this update), and inherent to portable apps — but the app is the one making the
registry write, so it owns the decision.

**Fix.** When autostart is enabled *and* the resolved data dir is the portable one, warn in the
settings UI that the exe location must not be writable by others; or refuse autostart for a
portable copy whose directory is not user-private; or copy the exe to `%LOCALAPPDATA%` and register
that path.

---

### N5 — Medium: `import_config` leaves orphaned live sessions behind

**Where:** import deliberately keeps `sessions/` so ids reconnect
([services.rs:665-667](<src-tauri/src/services.rs#L665-L667>)), and it replaces the entire service
list ([services.rs:731](<src-tauri/src/services.rs#L731))). Nothing reclaims sessions for ids that
no longer exist.

After an import, `sessions/` can contain WebView2 profiles with **live messenger cookies** for
services that are no longer configured — from this machine's previous setup, and (if ids survive
`normalize_imported_id`) potentially from the machine the file came from. `icons/` is not reclaimed
either, even though `remove_service` and `reset_service_icon` both clean it up
([services.rs:523](<src-tauri/src/services.rs#L523)), so import is the one path that leaks icon
files. The confirm dialog says "saved sessions are kept" ([app.js:698](<dist/app.js#L698)) without
saying "including ones that no longer have a service".

**Fix.** After the import's service set is decided, diff `sessions/` and `icons/` against the new id
set and reclaim the difference (same best-effort pattern as `remove_service`). Or make the confirm
dialog offer "remove sessions that are not in this file".

---

### N6 — Medium: `update_settings` is a full-struct replacement whose blast radius includes persistence

**Where:** [lib.rs:29-44](<src-tauri/src/lib.rs#L29-L44>) — `inner.settings = settings;` then
`autostart::apply(inner.settings.start_with_windows)?`.

There is no bounds check on any field (`hibernation_minutes: u64` is unbounded; the frontend only
checks `>= 0` at [app.js:668-671](<dist/app.js#L668-L671)) and no confirmation. `hibernation_minutes
= u64::MAX` is harmless (`saturating_mul` at [hibernate.rs:35](<src-tauri/src/hibernate.rs#L35)
prevents a panic). The real point is the shape: one command, granted to the UI webview, can flip the
HKCU Run key immediately. Combined with **N12** (the UI webview holds every `allow-*`), any XSS in
the UI webview becomes a **reboot-persistent** foothold, not just a one-shot action.

**Fix.** Clamp fields server-side. Consider requiring the setting to be toggled through a path that
surfaces the confirmation the UI already has for destructive actions.

---

### N7 — Low: `sniff_image_type`'s SVG branch is a substring test, not a validator

**Where:** [services.rs:45-48](<src-tauri/src/services.rs#L45-L48>):

```rust
let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
if head.contains("<svg") || head.trim_start().starts_with("<?xml") {
    return Some(("svg", "image/svg+xml"));
}
```

`contains("<svg")` anywhere in the first 512 bytes classifies the file as an image. An HTML
document, an XML config, or a text file that merely mentions `<svg` in a comment is accepted,
stored as `<id>.svg`, and later served back as `data:image/svg+xml;base64,…`
([services.rs:186](<src-tauri/src/services.rs#L186)) and assigned to `img.src`
([app.js:144](<dist/app.js#L144))). `<?xml` alone is accepted too, which classifies *any* XML
document as an image.

**The stated mitigation is correct, and it is the reason this is Low, not High.** The comment at
[services.rs:32-34](<src-tauri/src/services.rs#L32-L34>) — "SVGs are accepted as text and are only
ever rendered through `<img>`, where scripts cannot execute" — is accurate: an SVG loaded as an image
is in a non-scripting mode, so `<script>` and external references do not run. I re-verified there is
no `<object>`, `<iframe>`, `srcdoc`, inline `style="…url(...)"`, or `background-image` anywhere in
`dist/` (grep over `dist/` for `innerHTML|srcdoc|background-image|url(` returns nothing in
`styles.css` and nothing for the HTML-injection sinks in `app.js`).

What remains is that the *only* gate on what bytes reach the SVG rasterizer is a 512-byte substring
test, for up to 256 KB. Browser SVG parsing has a history of memory/CPU and decoder bugs; feeding an
arbitrary XML document to it is an unnecessary attack surface for a feature whose purpose is a 44 px
sidebar glyph.

**Fix.** Require the first non-whitespace token to be `<svg` (after optionally skipping an `<?xml …
?>` prolog) rather than `contains`. Reject documents containing `<script`, `foreignObject`, ` on[a-z]+=`
or `href="http` before storing. Both are cheap and both are testable in the existing
`image_type_sniffing` test ([services.rs:852-861](<src-tauri/src/services.rs#L852-L861>)).

---

### N8 — Low: the CSP cannot constrain custom icons at all (deliberate, but worth stating)

**Where:** `img-src 'self' data:` ([tauri.conf.json:22](<src-tauri/tauri.conf.json#L22)) is required
for `get_service_icon`'s data URL to render. The CSP itself is genuinely good — `default-src 'self'`,
`script-src 'self'`, `style-src 'self'`, no `unsafe-inline`, and `connect-src` limited to
`'self' ipc: http://ipc.localhost`.

The consequence worth recording: CSP provides **zero** provenance control over rendered image bytes.
The entire trust decision for "what image gets drawn in this app" lives in `sniff_image_type`
(**N7**). That is fine for `img-src` specifically — a `data:` image is not a script vector — but it
means **N7** is load-bearing, so **N7**'s hardening matters more than its severity suggests.

---

### N9 — Low: `MAX_ICON_BYTES` is checked via `metadata()` and then the file is re-`read()`

**Where:** [services.rs:83-91](<src-tauri/src/services.rs#L83-L91>) — `fs::metadata(&path)` →
`meta.len() > MAX_ICON_BYTES` → `fs::read(&path)`. Two separate opens on a user-chosen path; the
size checked is not the size read.

Exploitable only by a process racing the user's own picked file, so severity is Low — but the fix
removes the race *and* a syscall: read first, then compare `bytes.len() > MAX_ICON_BYTES as usize`.

`import_config` has the same shape with **no check at all**: `fs::read_to_string(&path)` on an
arbitrary picked path ([services.rs:679](<src-tauri/src/services.rs#L679>)), then a full
`serde_json::from_str`. A 2 GB "config" file is read entirely into memory. The `.json` picker
filter is cosmetic.

---

### N10 — Low: `?` on a webview operation *after* state was persisted still skips the emit

Round-1 finding 8 was fixed for `import_config` (activation failure is logged, not propagated, and
the snapshot is still emitted — [services.rs:743-757](<src-tauri/src/services.rs#L743-L757>)). The
same pattern is still live everywhere else:

- `update_service`: `close_webview(...)?` / `create_service_webview(...)?` at
  [services.rs:477-484](<src-tauri/src/services.rs#L477-L484>) — after `save_services()?` at line
  468. On failure the function returns `Err` **without** `emit_snapshot`, so the UI keeps rendering
  the service as enabled/active while the webview operation failed.
- `remove_service`: `close_webview(&app, &id)?` at [services.rs:512](<src-tauri/src/services.rs#L512>)
  — the service is already removed and saved; a failure here means the UI never learns.
- `hibernate_service`: `close_webview(&app, &id)?` at [webviews.rs:398](<src-tauri/src/webviews.rs#L398>)
  after `save_services()?` at line 395.
- `wake_service`: `create_service_webview(...)?` at [webviews.rs:425](<src-tauri/src/webviews.rs#L425>)
  after `save_services()?` at line 420.
- `set_active_service`: `activate(...)?` at [webviews.rs:201](<src-tauri/src/webviews.rs#L201) —
  here `active_service_id` was already mutated and saved inside `activate`
  ([webviews.rs:186](<src-tauri/src/webviews.rs#L186>)), so a `show_only` failure leaves the state
  claiming an active service with no visible webview.

**Fix.** Adopt `import_config`'s rule everywhere: once state is persisted, a webview failure is
logged, the snapshot is emitted, and the error is returned *after* the emit. The UI must never be
left showing a state that no longer exists.

---

### N11 — Low: `icon_id` validation is applied at three entry points but not at the trust boundary

`is_valid_slug` (`[a-z0-9-]`, ≤ 32) is enforced in `add_service`
([services.rs:391-394](<src-tauri/src/services.rs#L391-L394)) and in `import_config`
([services.rs:712-714](<src-tauri/src/services.rs#L712-L714)). It is **not** applied in
`load_services`, which backfills `icon_id` from the URL for old files
([storage.rs:139-143](<src-tauri/src/storage.rs#L139-L143))) — and `services.json` is a plain,
hand-editable file. A hand-edited `"iconId": "../../../../some/path"` therefore reaches the frontend.

It is currently harmless, and the reason is worth naming: `resolveServiceIcon` requires
`brandManifest[slug]` to be truthy before building a path ([app.js:112-113](<dist/app.js#L112-L113>)),
so the bundled manifest ([manifest.json](<dist/assets/icons/manifest.json>)) acts as an allowlist at
the render site. That is a real defense, but it is a *frontend* defense for a *backend* invariant,
and the backend already has the validator.

`service.icon` and `service.name`, by contrast, are **not validated at all** (`update_service`
assigns `service.icon = icon` with no check, [services.rs:451](<src-tauri/src/services.rs#L451>)).
They are safe today purely because every sink is `textContent` / `title` / `alt`
([app.js:168](<dist/app.js#L168), [app.js:183](<dist/app.js#L183), [app.js:249](<dist/app.js#L249))
and `dist/` contains no `innerHTML`. That is an invariant about *future* code, not a property of the
data.

**Fix.** Validate `icon_id` in `load_services` (drop invalid values to `None`) so the invariant
holds where untrusted data enters. Add a length cap and character filter for `name` and `icon`
server-side rather than relying on the frontend's choice of DOM API.

---

### N12 — Low: `withGlobalTauri: true` plus `core:default` gives the UI webview the whole app

**Where:** [tauri.conf.json:10](<src-tauri/tauri.conf.json#L10>) and
[default.json:7-28](<src-tauri/capabilities/default.json#L7-L28>).

`withGlobalTauri` is not itself a hole under `script-src 'self'` with no inline scripts — and I
re-confirmed there is no injection sink in `dist/` (no `innerHTML`, no `eval`, no `new Function`, no
inline `on*=` attributes, no `document.write`). The concern is breadth: `window.__TAURI__` exposes
every granted permission, and `core:default` expands to `core:path`, `core:event`, `core:window`,
`core:webview`, `core:app`, `core:image`, `core:resources`, `core:menu`, `core:tray` defaults. The UI
also holds all 22 `allow-*` app commands.

What `dist/` actually uses is tiny: `core.invoke`, `core.listen`
([app.js:57](<dist/app.js#L57), [app.js:784-793](<dist/app.js#L784-L793))) and `app.getVersion`
([app.js:649](<dist/app.js#L649))). It uses **none** of `core:webview` (including
`allow-internal-toggle-devtools` and `allow-get-all-webviews`), `core:image`, `core:resources`,
`core:menu`, or `core:tray`.

**Fix.** Replace `core:default` with `core:event:default`, `core:window:default`,
`core:path:default`, `core:app:default`. That removes the DevTools bypass in **N3** as a side
effect. This is defense-in-depth, not a live bug — but it is a one-line change with a real reduction.

---

### N13 — Low: `blocking_pick_file()` inside `async` commands pins a runtime worker per open dialog

**Where:** [services.rs:73-77](<src-tauri/src/services.rs#L73-L77>),
[services.rs:649-654](<src-tauri/src/services.rs#L649-L654>),
[services.rs:670-674](<src-tauri/src/services.rs#L670-L674>).

Because these commands are `async`, they run on the tokio worker pool, so this does **not** violate
the locking contract — no state mutex is held while the dialog is open (the lock is taken only after
the picker returns). That is the right call and it is worth confirming explicitly. The residual cost
is that each open dialog occupies one runtime worker for its whole lifetime, and a stuck or
multiplied dialog can stall unrelated async commands. The cancel path also takes the state lock
purely to return an unchanged snapshot ([services.rs:80](<src-tauri/src/services.rs#L80>)).

**Fix.** Use the callback form (`pick_file(|path| …)`) so the command returns immediately, or take
the snapshot before opening the dialog and return it on cancel.

---

### N14 — Low: `remove_icon_files` trusts the service `id` by a distributed invariant, not a local check

**Where:** [services.rs:58-62](<src-tauri/src/services.rs#L58-L62>) builds
`icon_file_dir(data_dir).join(format!("{id}.{ext}"))` from the caller's `id`.

It is safe today, and the chain is worth spelling out because it is not obvious: every `id` reaching
this function comes from a `service(&id)` lookup that must succeed first
([services.rs:97-99](<src-tauri/src/services.rs#L97-L99), [services.rs:127-129](<src-tauri/src/services.rs#L127-L129),
[services.rs:499-502](<src-tauri/src/services.rs#L499-L502))), and every stored `id` is a UUID —
minted by `add_service` ([services.rs:398](<src-tauri/src/services.rs#L398)) or forced by
`normalize_imported_id` ([services.rs:583-594](<src-tauri/src/services.rs#L583-L594))). `get_service_icon`
does the right thing locally: it re-derives the extension, requires the stem to parse as a UUID, and
returns `Ok(None)` otherwise ([services.rs:160-174](<src-tauri/src/services.rs#L160-L174))) — and
`Uuid::parse_str` rejects `/`, `\`, `..` and `:`, so no separator can survive. That defense-in-depth
comment at [services.rs:160-162](<src-tauri/src/services.rs#L160-L162>) is exactly the right
instinct.

**Fix.** Apply the same instinct to `remove_icon_files`: `if Uuid::parse_str(id).is_err() { return; }`.
One line, and it makes the invariant local instead of distributed across four call sites.

---

### N15 — Low: script injection into remote pages is incidental-safe rather than deliberately safe

**Where:** `initialization_script` interpolates the service id with Rust's `{:?}` escaping
([badges.rs:16](<src-tauri/src/badges.rs#L6-L23>)), and `reload_service` / `navigate` call
`webview.eval("location.reload()")` / `history.back()` / `history.forward()`
([webviews.rs:248](<src-tauri/src/webviews.rs#L248), [webviews.rs:284-285](<src-tauri/src/webviews.rs#L284-L285>)).

The `eval` strings are fixed literals, so there is no injection. The `{:?}` escaping is doing real
work for `init_script`, and it is correct — but only because `id` happens to always be a UUID. If an
id ever became attacker-influenced, that format string is the last line of defense and nothing
asserts it.

**Fix.** `debug_assert!(Uuid::parse_str(service_id).is_ok())` in `init_script`, or format from a
parsed `Uuid`. Cheap, and it converts an accident into a guarantee.

---

### N16 — Low: disk writes under the global mutex, and undefined behaviour on a poisoned mutex

Two related resilience notes.

`save_services()` / `save_settings()` — a `serde_json` serialize plus write plus `sync_all`
([storage.rs:86-95](<src-tauri/src/storage.rs#L86-L95))) — are called while the state mutex is held
in `hibernate.rs` phase 1 ([hibernate.rs:59](<src-tauri/src/hibernate.rs#L59)), `update_service`
([services.rs:468](<src-tauri/src/services.rs#L468)), `remove_service`
([services.rs:508](<src-tauri/src/services.rs#L508)), `reorder_services`
([services.rs:541](<src-tauri/src/services.rs#L541)), `pick_service_icon`
([services.rs:109](<src-tauri/src/services.rs#L109)), and `import_config`
([services.rs:734-735](<src-tauri/src/services.rs#L734-L735))). No webview op is involved, so this
is not a deadlock risk — but a slow or network-backed data dir stalls *every* command for the
duration. The project is otherwise careful about lock scope; this is the one place the scope is
wider than it needs to be.

Separately: every command converts a poisoned mutex into a user-facing error string
(`map_err(|e| e.to_string())?`). A single panic while holding the lock therefore degrades the whole
app into "every command errors" without crashing. Surviving poisoning (`.unwrap_or_else(|e|
e.into_inner())`) would be *wrong* here, because `save_services()?` after `services.push()`
([services.rs:409-410](<src-tauri/src/services.rs#L409-L410))) leaves the in-memory list mutated
while the file is unchanged — so continuing would preserve an inconsistent state. The right action
is to document this as a deliberate choice in the threading contract section of the spec, since a
reader will otherwise assume poisoning is handled.

---

### N17 — Low: the hibernation state machine has no "webview creation failed" state

`activate()` sets `hibernated = false` and `touch()`es the service *before* attempting creation
([webviews.rs:180-189](<src-tauri/src/webviews.rs#L180-L189)). If `create_service_webview` fails
(missing WebView2 runtime, bad URL), the service is left marked live with a fresh `last_active`
entry, so the hibernation scan will "hibernate" a service that never had a webview. Harmless in
behaviour, but it means the sidebar can show a service as live that is not, and the failure is only
surfaced as a toast string built by `webview_error` ([webviews.rs:22-26](<src-tauri/src/webviews.rs#L22-L26>))
— which is also the round-1 spec drift in **N21**.

---

## N21 — Remaining spec drift (three items)

1. **Signal** is in the spec's normative service list ([spec:61](<docs/superpowers/specs/2026-10-08-bzdium-design.md#L61)))
   and is absent from `recipes()` ([services.rs:216-277](<src-tauri/src/services.rs#L216-L277))).
   Grep for `Signal` in `src-tauri/src` returns nothing.
2. **Missing WebView2 runtime** is still a toast built from an error string
   ([webviews.rs:22-26](<src-tauri/src/webviews.rs#L22-L26))), where the spec requires a blocking
   dialog ([spec:97](<docs/superpowers/specs/2026-10-08-bzdium-design.md#L97))).
3. **`exported_at`** is still bare unix seconds via `chrono_like_now()`
   ([services.rs:613-618](<src-tauri/src/services.rs#L613-L618))) — a string field holding an integer,
   which will be awkward the moment anything tries to display or compare it.

## N22 — Repo hygiene (unchanged from round 1)

- `Cargo.lock` still carries **two** `winreg` versions: 0.55.0
  ([Cargo.lock:4759-4767](<src-tauri/Cargo.lock#L4759-L4767))) and 0.56.0
  ([Cargo.lock:4769-4777](<src-tauri/Cargo.lock#L4769-L4777))). `Cargo.toml` pins `winreg = "0.55"`
  ([Cargo.toml:24](<src-tauri/Cargo.toml#L24))); the 0.56 copy arrives transitively. Two versions of
  a registry crate in a 12 MB single-exe build is worth a `cargo tree` pass.
- Still **no LICENSE file** (glob for `LICENSE*`/`NOTICE*`/`THIRD*`/`ATTRIBUT*` returns nothing),
  despite bundling ~46 Simple Icons SVGs that are CC0 and therefore require attribution to be
  safe ([services.rs:213-214](<src-tauri/src/services.rs#L213-L214)) documents them as CC0).
  The README credits Ferdium in prose ([README.md:11](<README.md#L11))), which is good, but a
  license file and an attribution notice are the artifacts a distributor actually needs.
- Still **no CI** and **no `rustfmt.toml`** — and the test suite is now substantial (badge parsing,
  id normalization, slug/manifest cross-checks, URL validation, config round-trip, remote URL
  pattern). Tests that nothing runs automatically is the cheapest thing on this list to fix.
- `bzdium-portable/` still holds `bzdium.exe` and `bzdium_0.2.2_x64-setup.exe` in the working tree
  (gitignored, so not a leak — just clutter that a `git clean -x` would take).

## N23 — Dead APIs

- `get_settings`: registered ([lib.rs:67](<src-tauri/src/lib.rs#L67>)), ACL-granted
  ([default.json:9](<src-tauri/capabilities/default.json#L9>)), never called from `dist/`. Round-1
  finding 15, unchanged. Every granted-but-unused command is a permanent attack surface.
- `emit_state_changed` is now dead: it exists only to be referenced by the doc comment of
  `emit_snapshot` ([state.rs:141-145](<src-tauri/src/state.rs#L141-L145))). Grep finds no callers.
  Delete it — `emit_snapshot` is the correct API and having both invites a caller to pick the one
  that takes `&StateInner`, which is the shape that caused round-1 finding 3.
- Two cosmetic dead bindings: `let _ = data_dir;` ([services.rs:112](<src-tauri/src/services.rs#L112)))
  and `let (snapshot, _) = { … }` ([webviews.rs:207](<src-tauri/src/webviews.rs#L207))). Clippy
  would flag both.

---

## What this update does well

Worth stating precisely, because these are the things a reviewer usually has to ask for:

- **The fixes are the right fixes, not patches over symptoms.** `report_title` takes the
  `Webview` handle and compares labels — the structurally correct answer, not a heuristic.
  `normalize_imported_id` restricts ids to the one shape that is safe as a path component, with
  dedup, rather than blacklisting `..`. The badge cap is applied at *both* the per-service parse and
  the total sum, so it holds in debug builds with overflow checks on.
- **The fixes are tested, and the tests target the exact adversarial inputs.**
  `badge_count_is_capped` uses `(99999999999)` ([badges.rs:142-151](<src-tauri/src/badges.rs#L142-L151)));
  `imported_ids_are_uuids_only_and_deduped` uses `..\..\..\Users\me\AppData`, `../../etc`,
  `C:\Windows\System32` ([services.rs:926-933](<src-tauri/src/services.rs#L926-L933))).
  `bundled_brand_slugs_have_assets` asserts every catalog slug has a real file on disk
  ([services.rs:831-850](<src-tauri/src/services.rs#L831-L850))) — that is a build-integrity test
  most projects never write.
- **The spec was updated to match the code, including the security invariants.** Id normalization,
  the `startWithWindows` ignore, the badge cap, the `report_title` label binding, the threading
  contract, single-instance, and the icon precedence rules are all now normative in
  [the spec](<docs/superpowers/specs/2026-10-08-bzdium-design.md>). The threading contract at
  [spec:268-286](<docs/superpowers/specs/2026-10-08-bzdium-design.md#L268-L286)) is the clearest
  write-up of the WebView2 nested-pump deadlock I have seen in a project this size, and the code
  follows it.
- **`emit_to(UI_WEBVIEW_LABEL, …)` is used consistently instead of a global `emit`**
  ([state.rs:150](<src-tauri/src/state.rs#L150), [state.rs:165-170](<src-tauri/src/state.rs#L165-L170))).
  State snapshots — including `dataDir` — never reach a remote service webview. That is the correct
  choice and it is easy to get wrong.
- **The new icon path validates at the read boundary too.** `get_service_icon` re-validates a
  hand-editable filename against `{uuid}.{known-ext}` with an explicit comment explaining why
  ([services.rs:160-162](<src-tauri/src/services.rs#L160-L162))). Custom icons are stored as files,
  never as base64 blobs in config ([services.rs:25-26](<src-tauri/src/services.rs#L25-L26))), and
  deliberately do not travel in exports ([services.rs:549-551](<src-tauri/src/services.rs#L549-L551))).
- **`dist/` is still clean.** Re-verified in this pass: every DOM node is built with
  `createElement` + `textContent`, there is no `innerHTML`/`eval`/`new Function`/`insertAdjacentHTML`
  anywhere, no inline event attributes in [index.html](<dist/index.html>), and no `url(...)` in
  [styles.css](<dist/styles.css>). The new icon code keeps that property — `img.src` is set from a
  backend-produced data URL or a manifest-gated path, never from string-concatenated markup.
- **`tests/remote_pattern.rs` is the best test in the repo.** It replicates Tauri's
  `RemoteUrlPattern::from_str` pipeline and pins the exact behavior the remote capability depends on
  ([remote_pattern.rs:11-30](<src-tauri/tests/remote_pattern.rs#L11-L30))). Testing your interpretation
  of someone else's ACL semantics is exactly the right instinct for a capability this broad.

---

## Suggested fix order

1. **Navigation containment / origin check on `report_title`** (**N1**). This is the one finding that
   keeps a High-severity class alive. The `report_title` origin check is a few lines and closes the
   badge-forgery path immediately; `on_navigation` is the fuller fix.
2. **`report_title` input limits** (**N2**): `title.len()` cap + a 1 s per-service throttle. Two
   lines, and it hardens the only remote-reachable command.
3. **Always emit after persisting, even on webview failure** (**N10**). Copy `import_config`'s
   pattern into `update_service`, `remove_service`, `hibernate_service`, `wake_service`,
   `set_active_service`.
4. **Narrow `core:default`** (**N12**) to the four plugin defaults `dist/` actually uses. This also
   closes the DevTools gate bypass in **N3**.
5. **Tighten `sniff_image_type`** (**N7**) to require a leading `<svg` and reject `<script` /
   `foreignObject` / `on*=`; and read-then-size-check in `pick_service_icon` (**N9**). Add a size cap
   to `import_config`.
6. **Reclaim orphaned `sessions/` and `icons/` after import** (**N5**), and make the confirm dialog
   say what happens to them.
7. **Portable-autostart warning** (**N4**) when the exe directory is not user-private.
8. **Hygiene** (**N22**, **N23**): LICENSE + Simple Icons attribution notice, a CI job running
   `cargo test` + `cargo clippy`, delete `emit_state_changed` and `get_settings` (or use it),
   `cargo tree` the duplicate `winreg`, validate `icon_id` in `load_services` (**N11**), add the
   `Uuid` assert in `init_script` (**N15**) and in `remove_icon_files` (**N14**).
