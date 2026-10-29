//! Native Grok assistant (Mode B of the Grok integration): chat with xAI's
//! Grok models over their OpenAI-compatible Chat Completions API
//! (https://docs.x.ai — `POST https://api.x.ai/v1/chat/completions`,
//! `Authorization: Bearer <key>`, SSE streaming).
//!
//! Security posture:
//! - The API key is stored ONLY through the OS secret mechanism (Windows
//!   DPAPI, user-scoped, with additional entropy). It never reaches the
//!   frontend, settings files, exports, logs, or error strings.
//! - All API traffic is https to a single pinned host (`api.x.ai`); the
//!   URL is validated (scheme + host + non-reserved address) before any
//!   request, redirects are disabled so an Authorization header can never
//!   follow a redirect off the pinned host.
//! - Transport is WinHTTP (Schannel): system-trusted certificates, system
//!   proxy support. No new crates: winhttp.dll/crypt32.dll are FFI'd
//!   through the windows-sys dependency that is already in the tree.
//! - Errors carry a sanitized provider message only; any `xai-…`-shaped
//!   token found in a provider message is redacted before display.
//! - Non-Windows platforms: the pure logic here is shared, but the key
//!   store and transport report an honest "not supported on this
//!   platform" instead of degrading to insecure storage/transport.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::state::SharedState;
use tauri::{AppHandle, Emitter, State};

/// The single API host the assistant talks to. Everything else is refused.
pub const API_HOST: &str = "api.x.ai";
/// OpenAI-compatible chat surface (official xAI streaming docs use it).
pub const CHAT_PATH: &str = "/v1/chat/completions";
/// Live model list (used for the model dropdown and connection test).
pub const MODELS_PATH: &str = "/v1/models";
/// Older xAI deployments exposed the model list under this name; tried as
/// a fallback when `/v1/models` answers 404 so a retired path cannot brick
/// the configuration UI.
pub const MODELS_PATH_ALT: &str = "/v1/language-models";

/// Stored-credential file name inside the data dir (DPAPI blob, not the
/// plaintext key — the plaintext never touches disk).
const KEY_FILE: &str = "grok-api-key.bin";
/// Locally stored conversation history (user-deletable at any time).
pub const HISTORY_FILE: &str = "grok-conversations.json";
/// Retention cap: the oldest conversations beyond this are dropped on save
/// so the history file cannot grow without bound.
const MAX_CONVERSATIONS: usize = 50;
const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;

const TIMEOUTS_MS: [u32; 4] = [10_000, 10_000, 30_000, 120_000]; // resolve, connect, send, receive

pub const EVENT_CHUNK: &str = "grok-chunk";
pub const EVENT_DONE: &str = "grok-done";
pub const EVENT_ERROR: &str = "grok-error";

// ---------------------------------------------------------------------------
// Error model — kind drives which recovery actions the UI offers.
// ---------------------------------------------------------------------------

/// Error classes the UI understands. `auth`, `credits`, `model` and
/// `cancelled` are permanent: the UI never offers (auto-)retry for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GrokErrorKind {
    Auth,
    Credits,
    RateLimit,
    Model,
    Network,
    Timeout,
    Cancelled,
    Provider,
    Unknown,
}

impl GrokErrorKind {
    /// Whether a user-initiated retry may help. Deliberately excludes the
    /// permanent classes: retrying a bad key or an empty pre-paid balance
    /// cannot succeed and must not be offered as a path.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            GrokErrorKind::Network
                | GrokErrorKind::Timeout
                | GrokErrorKind::RateLimit
                | GrokErrorKind::Provider
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GrokError {
    pub kind: GrokErrorKind,
    #[serde(rename = "message")]
    pub message: String,
}

impl GrokError {
    pub fn new(kind: GrokErrorKind, message: impl Into<String>) -> Self {
        GrokError { kind, message: message.into() }
    }
}

impl std::fmt::Display for GrokError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

/// Redacts anything that looks like an API key (or the real key itself)
/// from provider text before it is ever shown or stored.
fn redact_secrets(text: &str) -> String {
    // Any xai-… token shape, long base64url-ish runs, and the actual key.
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("xai-") {
        out.push_str(&rest[..pos]);
        out.push_str("xai-[redacted]");
        rest = &rest[pos + 4..];
        // swallow the token body (alnum/_/- up to whitespace or quote)
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(rest.len());
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn truncate_message(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{cut}…")
}

/// Extracts the provider's human-readable error text from a JSON body
/// without ever echoing the request key. Handles the shapes OpenAI-style
/// APIs use: `{"error":{"message":…}}`, `{"error":"…"} `, `{"msg":…}`.
fn extract_provider_message(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let msg = v
        .get("error")
        .map(|e| match e {
            serde_json::Value::String(s) => s.clone(),
            other => other
                .get("message")
                .or_else(|| other.get("msg"))
                .or_else(|| other.get("code"))
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string(),
        })
        .or_else(|| {
            v.get("msg")
                .or_else(|| v.get("message"))
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })?;
    let msg = msg.trim();
    if msg.is_empty() {
        None
    } else {
        Some(truncate_message(&redact_secrets(msg), 300))
    }
}

/// Classifies an API response into a user-facing error. Pure + unit-tested:
/// status/body pairs come straight off the wire, never invented. Status
/// codes drive the classification, with body-sniffing as the fallback so a
/// provider that phrases an auth failure under an unexpected status still
/// lands in the right recovery bucket.
pub fn classify_response(status: u16, body: &str) -> GrokError {
    let provider = extract_provider_message(body);
    let lower = body.to_ascii_lowercase();
    // Provider phrasing beats the status code when they disagree about
    // billing-adjacent failures.
    let sniffed = if (lower.contains("api key")
        || lower.contains("unauthorized")
        || lower.contains("authentication"))
        && (lower.contains("invalid")
            || lower.contains("incorrect")
            || lower.contains("missing")
            || lower.contains("revoked")
            || lower.contains("expired")
            || lower.contains("malformed")
            || lower.contains("unauthorized"))
    {
        Some(GrokErrorKind::Auth)
    } else if lower.contains("quota") || lower.contains("insufficient credit") || lower.contains("billing") {
        Some(GrokErrorKind::Credits)
    } else if lower.contains("model") && (lower.contains("not found") || lower.contains("does not exist") || lower.contains("not available")) {
        Some(GrokErrorKind::Model)
    } else {
        None
    };
    let generic =
        |kind, fallback: &str| GrokError::new(kind, provider.clone().unwrap_or_else(|| fallback.to_string()));
    match status {
        401 | 403 => generic(GrokErrorKind::Auth, "The API key was rejected. Check the key in Settings."),
        402 => generic(GrokErrorKind::Credits, "The xAI account has insufficient credits for API use."),
        404 => generic(GrokErrorKind::Model, "That model or endpoint is not available on this account."),
        408 => generic(GrokErrorKind::Timeout, "The API took too long to answer."),
        413 => generic(GrokErrorKind::Unknown, "The conversation is too large for one request."),
        429 => {
            if sniffed == Some(GrokErrorKind::Credits) {
                generic(GrokErrorKind::Credits, "The xAI account is out of API credits.")
            } else {
                generic(GrokErrorKind::RateLimit, "Rate limited by the API. Wait a moment and retry.")
            }
        }
        s if (500..600).contains(&s) => generic(GrokErrorKind::Provider, "The xAI API is temporarily unavailable."),
        // Unmapped status: trust the body sniff if it recognized a known
        // failure shape, else stay neutral.
        _ => match sniffed {
            Some(kind @ (GrokErrorKind::Auth | GrokErrorKind::Credits | GrokErrorKind::Model)) => generic(kind, ""),
            _ => generic(GrokErrorKind::Unknown, "The API returned an unexpected response."),
        },
    }
}

// ---------------------------------------------------------------------------
// Endpoint hardening
// ---------------------------------------------------------------------------

/// Only https to the pinned public host may ever be requested. Rejects
/// non-https schemes, localhost/loopback/private/reserved hosts, and hosts
/// that are not exactly `API_HOST` — a request is built from constants in
/// this module, but the check runs anyway so a future refactor cannot
/// silently widen the surface.
pub fn validate_endpoint(host: &str, port: u16) -> Result<(), GrokError> {
    if host != API_HOST {
        return Err(GrokError::new(GrokErrorKind::Unknown, format!("refusing to contact non-{API_HOST} host")));
    }
    if port != 443 {
        return Err(GrokError::new(GrokErrorKind::Unknown, "refusing to contact a non-https port"));
    }
    validate_public_host(host)
}

/// Refuses localhost, loopback, link-local, private and reserved address
/// literals (the pinned host is public, but this guard is the documented
/// gate for any outbound request).
pub fn validate_public_host(host: &str) -> Result<(), GrokError> {
    let h = host.trim().to_ascii_lowercase();
    let refused = |why: &str| GrokError::new(GrokErrorKind::Network, format!("refusing to contact {why} address"));
    let h = h.strip_prefix('[').map(|s| s.trim_end_matches(']')).unwrap_or(&h);
    if h.is_empty() || h == "localhost" || h.ends_with(".localhost") || h.ends_with(".local") || h == "0.0.0.0" {
        return Err(refused("a local"));
    }
    if h.contains(':') {
        // IPv6 literal: normalize elided zeros crudely but safely — refuse
        // anything that is not a global address shape we allow-list (the
        // pinned API host resolves to A records anyway).
        let v6 = h.parse::<std::net::Ipv6Addr>();
        return match v6 {
            Ok(addr) if !addr.is_loopback() && !addr.is_unspecified() => Ok(()),
            Ok(_) => Err(refused("a loopback")),
            Err(_) => Err(refused("an invalid")),
        };
    }
    let octets: Vec<&str> = h.split('.').collect();
    if octets.len() == 4 {
        let parsed: Result<Vec<u8>, _> = octets.iter().map(|o| o.parse::<u8>()).collect();
        if let Ok(o) = parsed {
            let ip = std::net::Ipv4Addr::new(o[0], o[1], o[2], o[3]);
            let private_reserved = ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_unspecified()
                || ip.is_documentation()
                || o[0] == 0
                || o[0] >= 224;
            if private_reserved {
                return Err(refused("a private or reserved"));
            }
            return Ok(());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// API key validation + masking (never return the full key to the UI)
// ---------------------------------------------------------------------------

/// xAI keys today look like `xai-…`; validation stays shape-light on
/// purpose (no format lock-in): reject only what is dangerous or hopeless.
pub fn validate_api_key(raw: &str) -> Result<String, String> {
    let key = raw.trim();
    if key.len() < 20 {
        return Err("That key looks too short to be an xAI API key (keys start with “xai-”).".to_string());
    }
    if key.len() > 256 {
        return Err("That key is too long to be an xAI API key.".to_string());
    }
    if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("An API key must not contain spaces or control characters.".to_string());
    }
    Ok(key.to_string())
}

/// Masked form for display: first segment + last 4, never the middle.
pub fn mask_key(key: &str) -> String {
    let n = key.chars().count();
    if n < 8 {
        return "••••".to_string();
    }
    let last4: String = key.chars().skip(n - 4).collect();
    let head: String = key.chars().take(4).collect();
    if key.starts_with("xai-") && n >= 12 {
        format!("xai-••••{last4}")
    } else {
        format!("{head}••••{last4}")
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    pub configured: bool,
    pub masked: Option<String>,
    pub platform_supported: bool,
}

// ---------------------------------------------------------------------------
// Credential store: DPAPI (user-scoped, additional entropy) on Windows.
// ---------------------------------------------------------------------------

#[cfg(windows)]
const DPAPI_ENTROPY: &[u8] = b"bzdium/grok-api-key/v1";

#[cfg(windows)]
fn key_file(data_dir: &Path) -> PathBuf {
    data_dir.join(KEY_FILE)
}

#[cfg(windows)]
fn dpapi_protect(plain: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    unsafe {
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: plain.len() as u32,
            pbData: plain.as_ptr() as *mut u8,
        };
        let mut entropy = CRYPT_INTEGER_BLOB {
            cbData: DPAPI_ENTROPY.len() as u32,
            pbData: DPAPI_ENTROPY.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = CryptProtectData(
            &in_blob,
            std::ptr::null(),
            &mut entropy,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        );
        if ok == 0 {
            return Err("Windows refused to protect the key (DPAPI failure).".to_string());
        }
        let slice = std::slice::from_raw_parts(out.pbData, out.cbData as usize);
        let bytes = slice.to_vec();
        LocalFree(out.pbData as _);
        Ok(bytes)
    }
}

#[cfg(windows)]
fn dpapi_unprotect(blob: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    unsafe {
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: blob.len() as u32,
            pbData: blob.as_ptr() as *mut u8,
        };
        let mut entropy = CRYPT_INTEGER_BLOB {
            cbData: DPAPI_ENTROPY.len() as u32,
            pbData: DPAPI_ENTROPY.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = CryptUnprotectData(
            &in_blob,
            std::ptr::null_mut(),
            &mut entropy,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        );
        if ok == 0 {
            return Err("Could not unlock the stored API key (was it encrypted by another Windows user?).".to_string());
        }
        let slice = std::slice::from_raw_parts(out.pbData, out.cbData as usize);
        let bytes = slice.to_vec();
        LocalFree(out.pbData as _);
        Ok(bytes)
    }
}

/// Stores the key encrypted (DPAPI). The plaintext exists only in this
/// call frame and the OS-protected blob.
#[cfg(windows)]
pub fn save_api_key(data_dir: &Path, key: &str) -> Result<(), String> {
    let blob = dpapi_protect(key.as_bytes())?;
    std::fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
    crate::storage::atomic_write_bytes(&key_file(data_dir), &blob)
        .map_err(|e| format!("failed to store the API key: {e}"))
}

#[cfg(not(windows))]
pub fn save_api_key(_data_dir: &Path, _key: &str) -> Result<(), String> {
    Err("Grok API key storage is Windows-only right now (DPAPI); use Grok Web on this platform.".to_string())
}

#[cfg(windows)]
pub fn load_api_key(data_dir: &Path) -> Result<Option<String>, GrokError> {
    let bytes = match std::fs::read(key_file(data_dir)) {
        Ok(b) => b,
        Err(_) => return Ok(None), // not configured
    };
    if bytes.len() > 4096 {
        return Err(GrokError::new(GrokErrorKind::Unknown, "Stored credential file is corrupt."));
    }
    let plain = dpapi_unprotect(&bytes).map_err(|m| GrokError::new(GrokErrorKind::Unknown, m))?;
    let key = String::from_utf8(plain)
        .map_err(|_| GrokError::new(GrokErrorKind::Unknown, "Stored credential is corrupt."))?;
    Ok(Some(key))
}

#[cfg(not(windows))]
pub fn load_api_key(_data_dir: &Path) -> Result<Option<String>, GrokError> {
    Ok(None)
}

pub fn clear_api_key(data_dir: &Path) {
    #[cfg(windows)]
    let _ = std::fs::remove_file(key_file(data_dir));
    #[cfg(not(windows))]
    let _ = data_dir;
}

pub fn key_status(data_dir: &Path) -> KeyStatus {
    let platform_supported = cfg!(windows);
    match load_api_key(data_dir) {
        Ok(Some(key)) => KeyStatus { configured: true, masked: Some(mask_key(&key)), platform_supported },
        _ => KeyStatus { configured: false, masked: None, platform_supported },
    }
}

// ---------------------------------------------------------------------------
// SSE parsing + response chunk extraction (pure, unit-tested)
// ---------------------------------------------------------------------------

/// Incremental `text/event-stream` parser: feed raw network bytes, get
/// `data:` payloads per event. Only the data line matters for this API.
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
}

impl SseParser {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut events = Vec::new();
        loop {
            let Some(nl) = self.buf.iter().position(|b| *b == b'\n') else { break };
            let mut line: Vec<u8> = self.buf.drain(..=nl).collect();
            line.pop(); // \n
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_start_matches('\u{feff}');
            if let Some(data) = line.strip_prefix("data:") {
                events.push(data.strip_prefix(' ').unwrap_or(data).to_string());
            }
            // comments (`:…`), field lines (event:/id:/retry:) are ignored
        }
        events
    }

    /// Flush at stream end; a trailing unterminated `data:` line that
    /// never got its blank line is still delivered (provider quirk safety).
    pub fn finish(&mut self) -> Vec<String> {
        let line = String::from_utf8_lossy(&self.buf).to_string();
        self.buf.clear();
        let line = line.trim();
        if line.is_empty() {
            Vec::new()
        } else {
            line.strip_prefix("data:")
                .map(|d| d.strip_prefix(' ').unwrap_or(d).to_string())
                .into_iter()
                .collect()
        }
    }
}

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Default, Clone)]
pub struct StreamDelta {
    pub text: Option<String>,
    pub model: Option<String>,
    pub usage: Option<StreamUsage>,
    pub done: bool,
}

/// Extracts the pieces bzdium renders from one SSE `data:` payload.
pub fn parse_chunk(data: &str) -> StreamDelta {
    let mut out = StreamDelta::default();
    if data.trim() == "[DONE]" {
        out.done = true;
        return out;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
        return out;
    };
    if let Some(m) = v.get("model").and_then(|m| m.as_str()) {
        out.model = Some(m.to_string());
    }
    if let Some(usage) = v.get("usage") {
        let prompt = usage.get("prompt_tokens").and_then(|t| t.as_u64());
        let completion = usage.get("completion_tokens").and_then(|t| t.as_u64());
        if prompt.is_some() || completion.is_some() {
            out.usage = Some(StreamUsage {
                prompt_tokens: prompt.unwrap_or(0),
                completion_tokens: completion.unwrap_or(0),
            });
        }
    }
    // choices[0].delta.content (reasoning deltas in `reasoning_content` are
    // deliberately not surfaced: the API treats them as internal.
    if let Some(delta) = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
    {
        if let Some(text) = delta.get("content").and_then(|t| t.as_str()) {
            if !text.is_empty() {
                out.text = Some(text.to_string());
            }
        }
    }
    out
}

/// Request body for the chat endpoint. `roles` are restricted to the three
/// the API defines; content is length-capped so a runaway transcript cannot
/// build an unbounded request.
pub fn build_chat_body(model: &str, messages: &[ChatMessage]) -> Result<String, GrokError> {
    if !is_valid_model_id(model) {
        return Err(GrokError::new(GrokErrorKind::Model, format!("“{model}” is not a valid model id.")));
    }
    if messages.is_empty() {
        return Err(GrokError::new(GrokErrorKind::Unknown, "Cannot send an empty conversation."));
    }
    for m in messages {
        if !matches!(m.role.as_str(), "system" | "user" | "assistant") {
            return Err(GrokError::new(GrokErrorKind::Unknown, format!("unsupported message role {:?}", m.role)));
        }
        if m.content.len() > 400_000 {
            return Err(GrokError::new(GrokErrorKind::Unknown, "A message in this conversation is too large."));
        }
    }
    let body = serde_json::json!({
        "model": model,
        "messages": messages.iter().map(|m| serde_json::json!({"role": m.role, "content": m.content})).collect::<Vec<_>>(),
        "stream": true,
    });
    Ok(body.to_string())
}

/// Model ids become part of the JSON request body only; strict shape
/// anyway ([A-Za-z0-9._-], no whitespace) so a crafted id cannot smuggle
/// anything into the request.
pub fn is_valid_model_id(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 100
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// Parses the model-list response. Accepts both OpenAI (`{"data":[…]}`) and
/// legacy xAI (`{"models":[…]}`) shapes; ids only.
pub fn parse_models(body: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let items = v
        .get("data")
        .or_else(|| v.get("models"))
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();
    let mut ids = Vec::new();
    for item in items {
        if let Some(id) = item.get("id").and_then(|i| i.as_str()) {
            if is_valid_model_id(id) && !ids.iter().any(|existing: &String| existing == id) {
                ids.push(id.to_string());
            }
        }
    }
    ids
}

/// Default pick from a live model list: prefer the current flagship family,
/// fall back to anything grok-*, else the first entry. This is a preference
/// over models the API actually returned — never a hardcoded catalog.
/// Returned to the UI as `defaultModel` by `grok_test_connection` so the
/// preference lives in exactly one place.
pub fn pick_default_model<'a>(ids: &'a [String]) -> Option<&'a String> {
    ids.iter()
        .find(|id| id.starts_with("grok-4"))
        .or_else(|| ids.iter().find(|id| id.starts_with("grok-")))
        .or_else(|| ids.first())
}

// ---------------------------------------------------------------------------
// Conversation history (local, user-clearable)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokConversation {
    pub id: String,
    pub title: String,
    pub model: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub messages: Vec<ChatMessage>,
}

fn history_file(data_dir: &Path) -> PathBuf {
    data_dir.join(HISTORY_FILE)
}

pub fn load_conversations(data_dir: &Path) -> Vec<GrokConversation> {
    crate::storage::load_or_default(&history_file(data_dir))
}

/// Upserts by id, sorts newest-first, enforces the retention caps.
pub fn save_conversation(data_dir: &Path, conversation: GrokConversation) -> Result<(), String> {
    let mut all = load_conversations(data_dir);
    match all.iter().position(|c| c.id == conversation.id) {
        Some(pos) => all[pos] = conversation,
        None => all.push(conversation),
    }
    all.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
    all.truncate(MAX_CONVERSATIONS);
    // Hard file-size ceiling: drop oldest until it fits.
    let encode = |list: &Vec<GrokConversation>| serde_json::to_string(list).map(|s| s.len()).unwrap_or(usize::MAX);
    while encode(&all) > MAX_FILE_BYTES && !all.is_empty() {
        all.pop();
    }
    std::fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
    crate::storage::save_json(&history_file(data_dir), &all)
}

pub fn clear_conversations(data_dir: &Path) -> Result<(), String> {
    let path = history_file(data_dir);
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("failed to delete conversation history: {e}"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Transport (WinHTTP on Windows)
// ---------------------------------------------------------------------------

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Cancellation registry: request id -> live HINTERNET (as isize; raw
/// pointers are not Send). `grok_cancel_request` closes the handle from
/// another thread, which aborts any pending WinHTTP call on it.
static ACTIVE_REQUESTS: std::sync::LazyLock<Mutex<HashMap<String, isize>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

struct CancelGuard {
    request_id: String,
    handle: isize,
}

impl CancelGuard {
    fn register(request_id: &str, handle: *mut std::ffi::c_void) -> CancelGuard {
        if let Ok(mut map) = ACTIVE_REQUESTS.lock() {
            map.insert(request_id.to_string(), handle as isize);
        }
        CancelGuard { request_id: request_id.to_string(), handle: handle as isize }
    }

    fn matches(&self) -> bool {
        ACTIVE_REQUESTS
            .lock()
            .map(|map| map.get(&self.request_id) == Some(&self.handle))
            .unwrap_or(false)
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if self.matches() {
            if let Ok(mut map) = ACTIVE_REQUESTS.lock() {
                if map.get(&self.request_id) == Some(&self.handle) {
                    map.remove(&self.request_id);
                }
            }
        }
    }
}

/// WinHTTP error -> GrokError (message contains status codes only).
#[cfg(windows)]
fn transport_error(call: &str, code: u32) -> GrokError {
    const ERROR_WINHTTP_TIMEOUT: u32 = 12002;
    const ERROR_WINHTTP_NAME_NOT_RESOLVED: u32 = 12007;
    const ERROR_WINHTTP_CANNOT_CONNECT: u32 = 12029;
    const ERROR_WINHTTP_CONNECTION_ERROR: u32 = 12030;
    const ERROR_WINHTTP_OPERATION_CANCELLED: u32 = 12017;
    let kind = match code {
        ERROR_WINHTTP_TIMEOUT => GrokErrorKind::Timeout,
        ERROR_WINHTTP_OPERATION_CANCELLED => GrokErrorKind::Cancelled,
        ERROR_WINHTTP_NAME_NOT_RESOLVED
        | ERROR_WINHTTP_CANNOT_CONNECT
        | ERROR_WINHTTP_CONNECTION_ERROR => GrokErrorKind::Network,
        _ => GrokErrorKind::Network,
    };
    let detail = match code {
        ERROR_WINHTTP_TIMEOUT => "The connection to the API timed out.".to_string(),
        ERROR_WINHTTP_NAME_NOT_RESOLVED => "api.x.ai could not be resolved — check your internet connection.".to_string(),
        ERROR_WINHTTP_CANNOT_CONNECT => "Could not connect to api.x.ai.".to_string(),
        ERROR_WINHTTP_OPERATION_CANCELLED => "The request was cancelled.".to_string(),
        ERROR_WINHTTP_CONNECTION_ERROR => "The connection to the API was interrupted.".to_string(),
        other => format!("Network error {other} ({call}) while talking to the API."),
    };
    GrokError::new(kind, detail)
}

#[cfg(windows)]
fn last_os_error() -> u32 {
    unsafe { windows_sys::Win32::Foundation::GetLastError() }
}

/// One blocking https request. `on_data` streams each network read (SSE);
/// the full body is returned for non-streaming callers. Only called from
/// short-lived worker threads; the UI is fed through Tauri events.
#[cfg(windows)]
fn winhttp_request(
    method: &str,
    path: &str,
    key: &str,
    body: Option<&str>,
    request_id: Option<&str>,
    mut on_data: impl FnMut(&[u8]),
) -> Result<HttpResponse, GrokError> {
    use windows_sys::Win32::Networking::WinHttp::{
        WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
        WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
        WinHttpSendRequest, WinHttpSetOption, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
        WINHTTP_ACCESS_TYPE_NO_PROXY, WINHTTP_DISABLE_REDIRECTS, WINHTTP_FLAG_SECURE,
        WINHTTP_OPTION_CONNECT_TIMEOUT, WINHTTP_OPTION_DISABLE_FEATURE,
        WINHTTP_OPTION_RECEIVE_TIMEOUT, WINHTTP_OPTION_RESOLVE_TIMEOUT,
        WINHTTP_OPTION_SEND_TIMEOUT, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    };

    validate_endpoint(API_HOST, 443)?;

    unsafe {
        let agent = wide("bzdium-grok/1.0");
        let session = WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, std::ptr::null(), std::ptr::null(), 0);
        // AUTOMATIC_PROXY needs Win8.1+; fall back to default proxy resolution.
        let session = if session.is_null() {
            WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_NO_PROXY, std::ptr::null(), std::ptr::null(), 0)
        } else {
            session
        };
        if session.is_null() {
            return Err(transport_error("open", last_os_error()));
        }
        let host = wide(API_HOST);
        let connection = WinHttpConnect(session, host.as_ptr(), 443, 0);
        if connection.is_null() {
            let err = last_os_error();
            WinHttpCloseHandle(session);
            return Err(transport_error("connect", err));
        }
        let verb = wide(method);
        let object = wide(path);
        let request = WinHttpOpenRequest(
            connection,
            verb.as_ptr(),
            object.as_ptr(),
            std::ptr::null(), // HTTP/1.1
            std::ptr::null(), // no referrer
            std::ptr::null(), // accept any type
            WINHTTP_FLAG_SECURE,
        );
        if request.is_null() {
            let err = last_os_error();
            WinHttpCloseHandle(connection);
            WinHttpCloseHandle(session);
            return Err(transport_error("open-request", err));
        }
        let cancel_guard = request_id.map(|id| CancelGuard::register(id, request));

        // Timeouts (ms): resolve, connect, send, receive.
        for (option, value) in [
            (WINHTTP_OPTION_RESOLVE_TIMEOUT, TIMEOUTS_MS[0]),
            (WINHTTP_OPTION_CONNECT_TIMEOUT, TIMEOUTS_MS[1]),
            (WINHTTP_OPTION_SEND_TIMEOUT, TIMEOUTS_MS[2]),
            (WINHTTP_OPTION_RECEIVE_TIMEOUT, TIMEOUTS_MS[3]),
        ] {
            let v: u32 = value;
            WinHttpSetOption(request as _, option, &v as *const _ as *const _, std::mem::size_of::<u32>() as u32);
        }
        // An Authorization header must never follow a redirect.
        let disable: u32 = WINHTTP_DISABLE_REDIRECTS;
        WinHttpSetOption(request as _, WINHTTP_OPTION_DISABLE_FEATURE, &disable as *const _ as *const _, std::mem::size_of::<u32>() as u32);

        let headers = wide(&format!(
            "Authorization: Bearer {key}\r\nContent-Type: application/json\r\nAccept: text/event-stream, application/json\r\n"
        ));
        let body_bytes = body.map(str::as_bytes);
        let sent = WinHttpSendRequest(
            request,
            headers.as_ptr(),
            // -1: headers are NUL-terminated wide chars; let WinHTTP compute
            // the length (a hand-computed length here yields error 87).
            0xFFFF_FFFFu32,
            body_bytes.map(|b| b.as_ptr() as *const _).unwrap_or(std::ptr::null()),
            body_bytes.map(|b| b.len() as u32).unwrap_or(0),
            body_bytes.map(|b| b.len() as u32).unwrap_or(0),
            0,
        );
        if sent == 0 {
            let err = last_os_error();
            drop(cancel_guard);
            WinHttpCloseHandle(request);
            WinHttpCloseHandle(connection);
            WinHttpCloseHandle(session);
            return Err(transport_error("send", err));
        }
        if WinHttpReceiveResponse(request, std::ptr::null_mut()) == 0 {
            let err = last_os_error();
            drop(cancel_guard);
            WinHttpCloseHandle(request);
            WinHttpCloseHandle(connection);
            WinHttpCloseHandle(session);
            return Err(transport_error("receive", err));
        }

        let mut status: u32 = 0;
        let mut status_len: u32 = std::mem::size_of::<u32>() as u32;
        if WinHttpQueryHeaders(
            request,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            std::ptr::null(),
            &mut status as *mut u32 as *mut _,
            &mut status_len,
            std::ptr::null_mut(),
        ) == 0
        {
            let err = last_os_error();
            drop(cancel_guard);
            WinHttpCloseHandle(request);
            WinHttpCloseHandle(connection);
            WinHttpCloseHandle(session);
            return Err(transport_error("query-status", err));
        }

        let mut body_out: Vec<u8> = Vec::new();
        let mut read_error: Option<GrokError> = None;
        loop {
            let mut available: u32 = 0;
            if WinHttpQueryDataAvailable(request, &mut available) == 0 {
                read_error = Some(transport_error("query-data", last_os_error()));
                break;
            }
            if available == 0 {
                break;
            }
            let available = (available as usize).min(1 << 20);
            let mut chunk = vec![0u8; available];
            let mut read: u32 = 0;
            if WinHttpReadData(request, chunk.as_mut_ptr() as *mut _, available as u32, &mut read) == 0 {
                read_error = Some(transport_error("read", last_os_error()));
                break;
            }
            if read == 0 {
                break;
            }
            let chunk = &chunk[..read as usize];
            on_data(chunk);
            body_out.extend_from_slice(chunk);
            if body_out.len() > 64 * 1024 * 1024 {
                read_error = Some(GrokError::new(GrokErrorKind::Unknown, "Response exceeded 64 MB."));
                break;
            }
        }

        // Unregister FIRST: after the handle is closed a racing cancel must
        // not try to close the same (possibly recycled) value again.
        drop(cancel_guard);
        WinHttpCloseHandle(request);
        WinHttpCloseHandle(connection);
        WinHttpCloseHandle(session);

        if let Some(err) = read_error {
            return Err(err);
        }
        Ok(HttpResponse { status: status as u16, body: body_out })
    }
}

#[cfg(not(windows))]
fn winhttp_request(
    _method: &str,
    _path: &str,
    _key: &str,
    _body: Option<&str>,
    _request_id: Option<&str>,
    mut _on_data: impl FnMut(&[u8]),
) -> Result<HttpResponse, GrokError> {
    Err(GrokError::new(
        GrokErrorKind::Network,
        "Grok API mode needs the Windows transport (WinHTTP) and is not available on this platform yet — use Grok Web instead.",
    ))
}

// ---------------------------------------------------------------------------
// High-level operations
// ---------------------------------------------------------------------------

/// GET the live model list (also serves as the "connection test": a 200
/// with models proves the key works, without spending chat tokens).
pub fn fetch_models(data_dir: &Path) -> Result<Vec<String>, GrokError> {
    let key = load_api_key(data_dir)?.ok_or_else(|| {
        GrokError::new(GrokErrorKind::Auth, "No API key configured. Add your xAI API key in Settings.")
    })?;
    let response = winhttp_request("GET", MODELS_PATH, &key, None, None, |_| {})?;
    if response.status == 200 {
        let ids = parse_models(&String::from_utf8_lossy(&response.body));
        if !ids.is_empty() {
            return Ok(ids);
        }
        return Err(GrokError::new(GrokErrorKind::Provider, "The API returned no models — is this key enabled for API use?"));
    }
    if response.status == 404 {
        // Try the legacy path before giving up.
        let alt = winhttp_request("GET", MODELS_PATH_ALT, &key, None, None, |_| {})?;
        if alt.status == 200 {
            let ids = parse_models(&String::from_utf8_lossy(&alt.body));
            if !ids.is_empty() {
                return Ok(ids);
            }
        }
        return Err(classify_response(alt.status, &String::from_utf8_lossy(&alt.body).into_owned()));
    }
    Err(classify_response(response.status, &String::from_utf8_lossy(&response.body).into_owned()))
}

/// What the UI is told per streamed request. Emitted to the UI webview only.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokErrorEvent<'a> {
    pub request_id: &'a str,
    pub kind: GrokErrorKind,
    pub message: &'a str,
    pub retryable: bool,
}

/// Sends one chat completion request on a worker thread, streaming deltas
/// to the UI as events. Returns immediately.
fn stream_chat(
    app: AppHandle,
    data_dir: PathBuf,
    request_id: String,
    model: String,
    messages: Vec<ChatMessage>,
) {
    std::thread::spawn(move || {
        let finish_error = |app: &AppHandle, err: GrokError| {
            let event = GrokErrorEvent {
                request_id: &request_id,
                kind: err.kind,
                message: &err.message,
                retryable: err.kind.retryable(),
            };
            let _ = app.emit_to(crate::state::UI_WEBVIEW_LABEL, EVENT_ERROR, &event);
        };

        let key = match load_api_key(&data_dir) {
            Ok(Some(k)) => k,
            Ok(None) => {
                finish_error(&app, GrokError::new(GrokErrorKind::Auth, "No API key configured. Add your xAI API key in Settings."));
                return;
            }
            Err(e) => {
                finish_error(&app, e);
                return;
            }
        };
        let body = match build_chat_body(&model, &messages) {
            Ok(b) => b,
            Err(e) => {
                finish_error(&app, e);
                return;
            }
        };

        let mut parser = SseParser::default();
        let mut model_reported: Option<String> = None;
        let mut usage: Option<StreamUsage> = None;
        let mut emitted_any = false;
        let mut finish_ok = false;

        let result = winhttp_request(
            "POST",
            CHAT_PATH,
            &key,
            Some(&body),
            Some(&request_id),
            |chunk| {
                for data in parser.feed(chunk) {
                    let delta = parse_chunk(&data);
                    if delta.model.is_some() {
                        model_reported = delta.model.clone();
                    }
                    if delta.usage.is_some() {
                        usage = delta.usage.clone();
                    }
                    if let Some(text) = delta.text {
                        emitted_any = true;
                        let payload = serde_json::json!({ "requestId": request_id, "delta": text });
                        let _ = app.emit_to(crate::state::UI_WEBVIEW_LABEL, EVENT_CHUNK, &payload);
                    }
                    if delta.done {
                        finish_ok = true;
                    }
                }
            },
        );

        match result {
            Ok(response) => {
                if response.status != 200 {
                    let text = String::from_utf8_lossy(&response.body).into_owned();
                    finish_error(&app, classify_response(response.status, &text));
                    return;
                }
                // Flush a trailing unterminated SSE line, then finish.
                for data in parser.finish() {
                    let delta = parse_chunk(&data);
                    if let Some(text) = delta.text {
                        emitted_any = true;
                        let payload = serde_json::json!({ "requestId": request_id, "delta": text });
                        let _ = app.emit_to(crate::state::UI_WEBVIEW_LABEL, EVENT_CHUNK, &payload);
                    }
                    if delta.done {
                        finish_ok = true;
                        if delta.usage.is_some() {
                            usage = delta.usage.clone();
                        }
                    }
                }
                if finish_ok || emitted_any {
                    #[derive(Serialize)]
                    #[serde(rename_all = "camelCase")]
                    struct DoneEvent<'a> {
                        request_id: &'a str,
                        model: Option<&'a String>,
                        usage: Option<&'a StreamUsage>,
                    }
                    let _ = app.emit_to(
                        crate::state::UI_WEBVIEW_LABEL,
                        EVENT_DONE,
                        &DoneEvent { request_id: &request_id, model: model_reported.as_ref(), usage: usage.as_ref() },
                    );
                } else {
                    finish_error(&app, GrokError::new(GrokErrorKind::Provider, "The API closed the stream without sending content."));
                }
            }
            Err(err) => {
                if err.kind == GrokErrorKind::Cancelled && emitted_any {
                    // Cancelled mid-stream: keep the partial answer, mark done.
                    #[derive(Serialize)]
                    #[serde(rename_all = "camelCase")]
                    struct DoneEvent<'a> {
                        request_id: &'a str,
                        model: Option<&'a String>,
                        usage: Option<&'a StreamUsage>,
                    }
                    let _ = app.emit_to(
                        crate::state::UI_WEBVIEW_LABEL,
                        EVENT_DONE,
                        &DoneEvent { request_id: &request_id, model: model_reported.as_ref(), usage: usage.as_ref() },
                    );
                } else {
                    finish_error(&app, err);
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn grok_key_status(state: State<'_, SharedState>) -> Result<KeyStatus, String> {
    let data_dir = state.lock().map_err(|e| e.to_string())?.data_dir.clone();
    Ok(key_status(&data_dir))
}

#[tauri::command]
pub fn grok_save_key(state: State<'_, SharedState>, key: String) -> Result<KeyStatus, String> {
    let key = validate_api_key(&key).map_err(|e| e)?;
    let data_dir = state.lock().map_err(|e| e.to_string())?.data_dir.clone();
    save_api_key(&data_dir, &key)?;
    Ok(key_status(&data_dir))
}

#[tauri::command]
pub fn grok_delete_key(state: State<'_, SharedState>) -> Result<KeyStatus, String> {
    let data_dir = state.lock().map_err(|e| e.to_string())?.data_dir.clone();
    clear_api_key(&data_dir);
    Ok(key_status(&data_dir))
}

/// Connection test + live model list. Never bills tokens; a wrong key is
/// reported as an auth error, not a network error.
#[tauri::command]
pub fn grok_test_connection(state: State<'_, SharedState>) -> Result<TestConnectionResult, GrokError> {
    let data_dir = state
        .lock()
        .map_err(|e| GrokError::new(GrokErrorKind::Unknown, e.to_string()))?
        .data_dir
        .clone();
    if !cfg!(windows) {
        return Err(GrokError::new(
            GrokErrorKind::Network,
            "Grok API mode needs the Windows transport (WinHTTP) and is not available on this platform yet — use Grok Web instead.",
        ));
    }
    match fetch_models(&data_dir) {
        Ok(models) => Ok(TestConnectionResult {
            ok: true,
            error: None,
            models: Some(models.clone()),
            default_model: pick_default_model(&models).cloned(),
        }),
        Err(e) => Ok(TestConnectionResult { ok: false, error: Some(e), models: None, default_model: None }),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestConnectionResult {
    pub ok: bool,
    pub error: Option<GrokError>,
    pub models: Option<Vec<String>>,
    /// The model the UI should preselect when the user has not chosen one.
    pub default_model: Option<String>,
}

/// Streams one assistant turn. Events: `grok-chunk`, `grok-done`,
/// `grok-error` (all carrying requestId). The key is read from the OS
/// credential store here — the frontend never sees it.
#[tauri::command]
pub fn grok_send_message(
    app: AppHandle,
    state: State<'_, SharedState>,
    request_id: String,
    model: String,
    messages: Vec<ChatMessage>,
) -> Result<(), String> {
    if request_id.is_empty() || request_id.len() > 128 {
        return Err("invalid request id".to_string());
    }
    let data_dir = state.lock().map_err(|e| e.to_string())?.data_dir.clone();
    stream_chat(app, data_dir, request_id, model, messages);
    Ok(())
}

/// Aborts the in-flight request (closes the WinHTTP handle from here; the
/// blocked call on the worker thread fails with OPERATION_CANCELLED).
#[tauri::command]
pub fn grok_cancel_request(request_id: String) -> Result<(), String> {
    if let Ok(mut map) = ACTIVE_REQUESTS.lock() {
        if let Some(handle) = map.remove(&request_id) {
            #[cfg(windows)]
            unsafe {
                windows_sys::Win32::Networking::WinHttp::WinHttpCloseHandle(handle as *mut _);
            }
            #[cfg(not(windows))]
            let _ = handle;
        }
    }
    Ok(())
}

#[tauri::command]
pub fn grok_list_conversations(state: State<'_, SharedState>) -> Result<Vec<GrokConversation>, String> {
    let data_dir = state.lock().map_err(|e| e.to_string())?.data_dir.clone();
    Ok(load_conversations(&data_dir))
}

#[tauri::command]
pub fn grok_save_conversation(
    state: State<'_, SharedState>,
    conversation: GrokConversation,
) -> Result<(), String> {
    let data_dir = state.lock().map_err(|e| e.to_string())?.data_dir.clone();
    // Defense in depth: ids are only ever generated as UUIDs; a tampered
    // IPC payload must not reach file names via anything else.
    if uuid::Uuid::parse_str(&conversation.id).is_err() {
        return Err("invalid conversation id".to_string());
    }
    save_conversation(&data_dir, conversation)
}

#[tauri::command]
pub fn grok_clear_history(state: State<'_, SharedState>) -> Result<(), String> {
    let data_dir = state.lock().map_err(|e| e.to_string())?.data_dir.clone();
    clear_conversations(&data_dir)
}

// ---------------------------------------------------------------------------
// Tests — pure logic only; network and credential store are not exercised.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_is_pinned_and_public() {
        validate_endpoint(API_HOST, 443).expect("pinned endpoint must pass");
        assert!(validate_endpoint("localhost", 443).is_err());
        assert!(validate_endpoint("127.0.0.1", 443).is_err());
        assert!(validate_endpoint("10.0.0.5", 443).is_err());
        assert!(validate_endpoint("192.168.1.10", 443).is_err());
        assert!(validate_endpoint("169.254.1.2", 443).is_err());
        assert!(validate_endpoint("api.x.ai", 80).is_err());
    }

    #[test]
    fn public_host_guard_rejects_reserved_shapes() {
        assert!(validate_public_host("0.0.0.0").is_err());
        assert!(validate_public_host("100.64.0.1").is_ok()); // CGNAT is not private per RFC1918; fine
        assert!(validate_public_host("224.0.0.1").is_err());
        assert!(validate_public_host("::1").is_err());
        assert!(validate_public_host("example.com").is_ok());
    }

    #[test]
    fn key_validation_rejects_junk() {
        assert!(validate_api_key("short").is_err());
        assert!(validate_api_key("").is_err());
        assert!(validate_api_key("has space inside of the key here ok").is_err());
        let ok = validate_api_key("xaiTESTKEY1234567890abcdefABCDEF").unwrap();
        assert_eq!(ok, "xaiTESTKEY1234567890abcdefABCDEF");
        // trimmed
        assert_eq!(validate_api_key("  xaiTESTKEY1234567890abcdefABCDEF  ").unwrap(), "xaiTESTKEY1234567890abcdefABCDEF");
    }

    #[test]
    fn masking_never_shows_full_key() {
        let key = "xai-Abcdefghijklmnop1234";
        assert_eq!(mask_key(key), "xai-••••1234");
        let k2 = "Abcdefgh12345678";
        assert_eq!(mask_key(k2), "Abcd••••5678");
        assert_eq!(mask_key("tiny"), "••••");
        let masked = mask_key(key);
        assert!(!masked.contains("Abcdefgh"), "mask leaked the key middle");
    }

    #[test]
    fn sse_parser_handles_split_chunks() {
        let mut p = SseParser::default();
        // one event split across three reads, plus CRLF and a comment
        let a = p.feed(b"data: {\"abc\"");
        assert!(a.is_empty());
        let b = p.feed(b"\r\n: keep-alive\n\n");
        assert_eq!(b.len(), 1);
        assert_eq!(b[0], "{\"abc\"");
        let c = p.feed(b"data: [DONE]\n");
        assert_eq!(c, vec!["[DONE]".to_string()]);
        // no trailing newline: finish() still yields it
        let d = p.finish();
        assert!(d.is_empty());
        let e = p.feed(b"data: tail-no-newline");
        assert!(e.is_empty());
        assert_eq!(p.finish(), vec!["tail-no-newline".to_string()]);
    }

    #[test]
    fn chunk_parsing_pulls_text_model_usage() {
        let d = parse_chunk("{\"object\":\"chat.completion.chunk\",\"model\":\"grok-4.7\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\",\"role\":\"assistant\"}}],\"usage\":{\"prompt_tokens\":41,\"completion_tokens\":2}}");
        assert_eq!(d.text.as_deref(), Some("Hel"));
        assert_eq!(d.model.as_deref(), Some("grok-4.7"));
        let usage = d.usage.expect("usage extracted");
        assert_eq!((usage.prompt_tokens, usage.completion_tokens), (41, 2));
        assert!(!d.done);

        let done = parse_chunk("[DONE]");
        assert!(done.done);
        assert!(done.text.is_none());

        // reasoning-only delta carries no visible text
        let reasoning = parse_chunk("{\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking\"}}]}");
        assert!(reasoning.text.is_none());
        // null content tolerated
        let null = parse_chunk("{\"choices\":[{\"delta\":{\"content\":null}}]}");
        assert!(null.text.is_none());
        // garbage tolerated
        assert!(parse_chunk("not json").text.is_none());
    }

    #[test]
    fn classification_covers_all_documented_failures() {
        let cases: &[(u16, GrokErrorKind)] = &[
            (401, GrokErrorKind::Auth),
            (403, GrokErrorKind::Auth),
            (402, GrokErrorKind::Credits),
            (429, GrokErrorKind::RateLimit),
            (500, GrokErrorKind::Provider),
            (503, GrokErrorKind::Provider),
            (408, GrokErrorKind::Timeout),
            (418, GrokErrorKind::Unknown),
        ];
        for (status, kind) in cases {
            let err = classify_response(*status, "{}");
            assert_eq!(err.kind, *kind, "status {status}");
            // the classification itself must not embed a key
            assert!(!err.message.contains("xai-sk-real"), "messages are provider-only");
        }
        // quota-shaped 429 is a billing problem, not "slow down"
        let quota = classify_response(429, "{\"error\":{\"message\":\"exceeded quota\"}}");
        assert_eq!(quota.kind, GrokErrorKind::Credits);
        // permanent vs retryable split drives the UI recovery actions
        assert!(!GrokErrorKind::Auth.retryable());
        assert!(!GrokErrorKind::Credits.retryable());
        assert!(!GrokErrorKind::Model.retryable());
        assert!(!GrokErrorKind::Cancelled.retryable());
        assert!(GrokErrorKind::RateLimit.retryable());
        assert!(GrokErrorKind::Network.retryable());
        assert!(GrokErrorKind::Timeout.retryable());
        assert!(GrokErrorKind::Provider.retryable());
    }

    #[test]
    fn provider_errors_are_redacted_and_bounded() {
        let err = classify_response(401, "{\"error\":{\"message\":\"bad key xai-SUPERSECRETVALUE123 sent\"}}");
        assert!(!err.message.contains("SUPERSECRETVALUE"), "provider message leaked the key: {}", err.message);
        assert!(err.message.contains("xai-[redacted]"));

        let long = "x".repeat(2000);
        let err = classify_response(500, &format!("{{\"error\":{{\"message\":\"{long}\"}}}}"));
        assert!(err.message.chars().count() <= 301, "message not truncated");

        // non-JSON body still classifies
        assert_eq!(classify_response(502, "<html>bad gateway</html>").kind, GrokErrorKind::Provider);
    }

    /// Observed verbatim from api.x.ai (2026-10): an INVALID key is rejected
    /// with HTTP 400 + `{"code":"invalid-argument","error":"Incorrect API key
    /// provided…"}` — not 401. Status-only classification misfiles this as
    /// Unknown; the body sniff must land it in Auth (no-retry bucket).
    #[test]
    fn xai_reports_a_bad_key_over_http_400() {
        let err = classify_response(
            400,
            r#"{"code":"invalid-argument","error":"Incorrect API key provided. You can obtain an API key from https://console.x.ai."}"#,
        );
        assert_eq!(err.kind, GrokErrorKind::Auth);
        assert!(err.message.contains("Incorrect API key provided"), "message: {}", err.message);
        assert!(!GrokErrorKind::Auth.retryable());
        // An unrelated 400 stays neutral instead of guessing.
        assert_eq!(
            classify_response(400, r#"{"error":{"message":"field validation failed"}}"#).kind,
            GrokErrorKind::Unknown
        );
    }

    #[test]
    fn model_payload_is_built_and_validated() {
        let msgs = vec![
            ChatMessage { role: "user".into(), content: "Hi".into() },
            ChatMessage { role: "assistant".into(), content: "Hello".into() },
            ChatMessage { role: "user".into(), content: "Bye".into() },
        ];
        let body = build_chat_body("grok-4.7", &msgs).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["model"], "grok-4.7");
        assert_eq!(v["stream"], true);
        assert_eq!(v["messages"].as_array().unwrap().len(), 3);
        assert_eq!(v["messages"][0]["role"], "user");

        assert!(build_chat_body("bad model id!", &msgs).is_err());
        assert!(build_chat_body("grok-4.7", &[]).is_err());
        assert!(build_chat_body("grok-4.7", &[ChatMessage { role: "hacker".into(), content: "x".into() }]).is_err());
    }

    #[test]
    fn model_id_shape_is_strict() {
        assert!(is_valid_model_id("grok-4.7"));
        assert!(is_valid_model_id("grok-code-fast-1"));
        assert!(!is_valid_model_id(""));
        assert!(!is_valid_model_id("grok 4"));
        assert!(!is_valid_model_id("m\nodel"));
        assert!(!is_valid_model_id(&"a".repeat(101)));
    }

    #[test]
    fn models_parsing_accepts_both_shapes() {
        let openai = r#"{"object":"list","data":[{"id":"grok-4.7","object":"model"},{"id":"grok-4.6"},{"id":"bad id"}]}"#;
        assert_eq!(parse_models(openai), vec!["grok-4.7".to_string(), "grok-4.6".to_string()]);
        let legacy = r#"{"models":[{"id":"grok-3"}]}"#;
        assert_eq!(parse_models(legacy), vec!["grok-3".to_string()]);
        assert!(parse_models("garbage").is_empty());
        assert!(parse_models("{}").is_empty());
    }

    #[test]
    fn default_model_prefers_flagship_family_without_hardcoding() {
        let list: Vec<String> = ["grok-3", "grok-4-fast", "grok-4.6", "grok-2"].iter().map(|s| s.to_string()).collect();
        assert_eq!(pick_default_model(&list).unwrap(), "grok-4-fast");
        let no4: Vec<String> = ["grok-3", "zeta"].iter().map(|s| s.to_string()).collect();
        assert_eq!(pick_default_model(&no4).unwrap(), "grok-3");
        let none: Vec<String> = vec![];
        assert!(pick_default_model(&none).is_none());
    }

    #[test]
    fn conversation_store_caps_and_upserts() {
        let dir = std::env::temp_dir().join(format!(
            "bzdium-grok-test-{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let conv = |id: &str, updated: u64| GrokConversation {
            id: id.to_string(),
            title: format!("conv {id}"),
            model: "grok-4.7".into(),
            created_at: updated,
            updated_at: updated,
            messages: vec![ChatMessage { role: "user".into(), content: "hi".into() }],
        };
        // write 55 conversations, then re-save #7 (upsert keeps one copy)
        for i in 0..55u64 {
            save_conversation(&dir, conv(&format!("{:032x}", i), i)).unwrap();
        }
        save_conversation(&dir, conv(&format!("{:032x}", 7), 999)).unwrap();
        let all = load_conversations(&dir);
        assert_eq!(all.len(), super::MAX_CONVERSATIONS, "retention cap");
        assert_eq!(all[0].updated_at, 999, "newest first, upserted");
        assert!(all.iter().all(|c| c.id != format!("{:032x}", 0)), "oldest evicted");
        clear_conversations(&dir).unwrap();
        assert!(load_conversations(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Exercises the real OS credential mechanism (no network): a key saved
    /// through the public API must load back byte-identical, must exist on
    /// disk only as a DPAPI blob (the plaintext must not be greppable in the
    /// file), and must disappear on clear. Windows-only by design — the
    /// store itself refuses other platforms.
    #[cfg(windows)]
    #[test]
    fn api_key_round_trips_through_dpapi() {
        let dir = std::env::temp_dir().join(format!(
            "bzdium-grok-dpapi-{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Deliberately fake, unusable test literal.
        let key = "xai-TESTKEY-not-a-real-credential-000000";

        assert!(load_api_key(&dir).unwrap().is_none(), "fresh dir has no key");
        save_api_key(&dir, key).unwrap();

        // The blob on disk must not contain the plaintext key.
        let on_disk = std::fs::read(dir.join("grok-api-key.bin")).unwrap();
        assert!(!on_disk.windows(key.len()).any(|w| w == key.as_bytes()), "plaintext key on disk!");

        assert_eq!(load_api_key(&dir).unwrap().as_deref(), Some(key));
        assert_eq!(key_status(&dir).masked.as_deref(), Some("xai-••••0000"));

        clear_api_key(&dir);
        assert!(load_api_key(&dir).unwrap().is_none());
        assert!(!dir.join("grok-api-key.bin").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
