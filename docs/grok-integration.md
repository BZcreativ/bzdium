# Grok integration (xAI) — implementation notes

Requirement six added Grok to bzdium's service catalog in two clearly
separated integration modes. This document records what was implemented,
how credentials are handled, and which provider restrictions remain.

## Integration modes

| | Mode A — **Grok** (web) | Mode B — **Grok API** (native assistant) |
|---|---|---|
| Catalog entry | "Grok", `https://grok.com/` | "Grok API", pinned to `https://grok.com/` |
| Backed by | An ordinary embedded webview | No webview — the UI webview renders the chat |
| Auth | Grok/X account sign-in on the page | xAI API key (console.x.ai), stored DPAPI-encrypted |
| Billing | Grok website / X subscription | xAI API credits, **billed separately** |

Both entries appear side by side in the AI section of the Add Service
panel, use the official Grok mark (`dist/assets/icons/grok.svg`, taken
from grok.com's own brand asset; chip color `#000000` in
`assets/icons/manifest.json`), and can be added, reordered, renamed,
disabled and removed like any other service without affecting others.
The two modes stay visibly distinct in the sidebar, context menus, and
Add-Service form; a Grok website subscription never implies API access
and vice versa — the UI states this wherever API use is offered.

### Mode A — Grok Web

`grok.com` loads through the standard service-view architecture
(https-only navigation, per-service session profile, watchdog,
"Open in browser" fallback in the context menu and via
`open_service_external`). No credentials are stored by bzdium, and no
subscription enforcement is bypassed.

### Mode B — Grok API (xAI)

Implements the OpenAI-compatible Chat Completions surface documented at
docs.x.ai: `POST https://api.x.ai/v1/chat/completions` with
`Authorization: Bearer`, `stream: true` over SSE
(`data: {chat.completion.chunk}` … `data: [DONE]`), and the live model
list (`GET /v1/models`, falling back to `/v1/language-models`).

Chat features: streaming responses with a generating indicator,
cancel (closes the in-flight WinHTTP handle), conversation history
stored locally (`grok-conversations.json`, capped at 50 conversations,
deletable from Settings or the context menu; also deleted when the
service is removed), model picker populated **from the API's live list**
(never hardcoded; a stored model that the list no longer contains is
shown as "not in current list"), default model recommendation computed
backend-side (flagship `grok-4*` family first over whatever the API
returned). Reasoning-model `reasoning_content` deltas are deliberately
not surfaced.

## Credential handling

- The key is entered once in Settings → **Grok assistant** (password
  field), validated (length, no whitespace/control chars), and stored
  **only** as a Windows DPAPI blob (`CryptProtectData`, user-scoped,
  additional entropy) in `<dataDir>/grok-api-key.bin`. The plaintext
  never touches disk, settings files, exports, logs, or the frontend —
  requests are built Rust-side in `src-tauri/src/grok.rs`.
- The UI only ever sees a masked form (`xai-••••1234`).
- Transport is WinHTTP (Schannel, system trust store, system proxy):
  https to the pinned host `api.x.ai` only, redirects disabled so the
  Authorization header cannot leave that origin, timeouts on every
  phase (10 s resolve/connect, 30 s send, 120 s receive-stall).
- Provider error messages are sanitized before display: `xai-…`-shaped
  tokens are redacted and messages truncated; error *classes* (auth,
  credits, rate-limit, model, network, timeout, cancelled, provider)
  drive which recovery actions are offered. Permanent failures (auth,
  credits, model) never offer Retry.

## Error handling

Shared surfaces: chat errors render in the assistant's error box with
only the relevant actions (Retry for transient kinds, Dismiss always,
"Configure API key…" callout when unauthenticated); website-mode network
failures are recoverable through the context menu's Reload / Open in
browser. WinHTTP status codes map to friendly text (name resolution,
timeout, connect refusal, cancellation).

## Platform and accessibility notes

- **API mode is Windows-only** for now: the credential store (DPAPI) and
  the https transport (WinHTTP) are Windows APIs chosen to avoid adding
  a TLS crate to the dependency tree (crate downloads are blocked in
  this environment). On other platforms the assistant reports the
  limitation honestly and Grok Web remains available. Grok Web works
  everywhere the app runs.
- Font sizes in the assistant UI are defined in `rem` and scale with
  `html { font-size: calc(16px * var(--text-scale, 1)) }` — the
  application-wide Text Size presets (Small/Default/Large/Extra Large
  from requirement two) drive exactly this variable, so the assistant
  follows them automatically. The rest of the app's chrome is still
  px-based and is requirement two's scope.
- Controls are native buttons/selects/textarea (keyboard-operable, with
  the app's existing focus-visible outlines), the message area is
  `role="log" aria-live="polite"`, the status indicator is
  `role="status"`, and Enter sends / Shift+Enter adds a newline.

## What was verified

- `cargo test`: 288 tests across the lib, agent-contract,
  agent-lifecycle and remote-pattern suites, including the new Grok
  tests (catalog shape, endpoint pinning, key validation/masking, SSE
  parsing on split chunks, chunk/usage extraction, error
  classification incl. quota-vs-rate-limit and retryability, request
  body/role validation, model-list shapes, default-model preference,
  conversation retention caps, `grokApi` serde round-trips, and a real
  DPAPI save/load/clear round-trip asserting the plaintext never lands
  on disk).
- **Live transport check** (`cargo test --test grok_live -- --ignored`,
  deliberately not part of the default suite): a request with a fake
  key against the real `api.x.ai` proves TLS, host pinning and the
  Bearer header end-to-end and must classify as an auth error. This
  caught two things that offline tests could not: `WinHttpSendRequest`
  rejects a hand-computed wide-header length with error 87 (fixed by
  letting WinHTTP compute it from NUL termination), and **xAI rejects
  an invalid key with HTTP 400 `invalid-argument`, not 401** — error
  classification therefore sniffs the response body as well as the
  status code, so a bad key always lands in the no-retry auth bucket.
- Browser preview of the real `dist/` UI (mocked backend): Add Service
  panel shows both entries with the official mark; the assistant
  surface opens with the billing note; a simulated streamed round-trip
  persists and retitles the conversation; transient errors show Retry
  while auth errors clear the retry path; `--text-scale: 1.3` scales
  the UI (heading 15.2 px → 19.76 px) without layout breakage; the
  Settings panel shows the masked key, connection state and live model
  dropdown; the assistant's context menu carries only applicable
  actions.
- A real streaming chat completion needs a funded key and is not
  exercised by tests or by the live check. Manual first run: add
  "Grok API" → Settings → Grok assistant → paste key → Test → chat.

## Remaining provider restrictions

- API billing is per-token on the user's xAI account; bzdium adds no
  markup and cannot work without credits. Website (Grok/X) Premium
  subscriptions do not include API credits.
- xAI retires models on a published schedule; the picker always
  reflects the live list and a retired selection degrades gracefully.
- Embedding grok.com (Mode A) is subject to xAI's site behavior; if the
  site refuses embedded sign-in flows, "Open in browser" is the
  supported fallback.
- No affiliation: Grok, xAI and their marks belong to xAI; the icon is
  bundling the provider's own published brand asset for identification.
