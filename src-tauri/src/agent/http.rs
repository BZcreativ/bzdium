//! Loopback REST transport for the agent layer.
//!
//! Built directly on hyper 1.x. No web framework, no router crate: the route
//! table is forty lines and every dependency avoided here is a dependency the
//! bzdium binary does not ship.
//!
//! THREAT MODEL (the reason this file is written the way it is)
//! -----------------------------------------------------------
//! bzdium hosts real remote web applications (WhatsApp, Gmail, Discord) in
//! WebView2 children of this very process. That means hostile JavaScript runs
//! in a context that can reach http://127.0.0.1:<port>. A loopback listener in
//! a browser-hosting app is therefore NOT a private channel; it is reachable
//! by attacker-controlled code. Every control below exists to break that path:
//!
//!  1. Bind 127.0.0.1 only. Never 0.0.0.0, never a LAN address.
//!  2. Reject any request that carries an Origin header. A browser attaches
//!     Origin to every cross-origin fetch; a native CLI client does not. This
//!     single check removes every browser-initiated caller, including a
//!     service webview calling its own loopback address.
//!  3. Validate the Host header against 127.0.0.1 / localhost. Defeats DNS
//!     rebinding, where an attacker A-record flips to 127.0.0.1 mid-session.
//!  4. Require a bearer token, compared in constant time (scope::TokenStore).
//!     This is the control that survives even if 2 and 3 are bypassed: a
//!     hostile page cannot read the token, and cannot send the Authorization
//!     header cross-origin without a preflight.
//!  5. Never answer a CORS preflight. OPTIONS returns 405 with no CORS
//!     headers, so the browser blocks the real request from ever being sent.
//!  6. Per-token rate limit, so a leaked token cannot be used as a busy loop
//!     against webview lifecycle operations.
//!
//! Each control is enforced independently and has its own test. Controls 2, 3
//! and 5 are checked BEFORE the body is read and BEFORE a tool is dispatched,
//! so an unauthenticated probe cannot spend server resources.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::header::{ALLOW, AUTHORIZATION, CONTENT_TYPE, HOST, ORIGIN, RETRY_AFTER, WWW_AUTHENTICATE};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};

use super::audit::{now_unix, record, redact, AuditEntry};
use super::error::{AgentError, ErrorCode};
use super::guard::dispatch;
use super::mcp;
use super::openapi::{self, API_PREFIX, MCP_PATH};
use super::prompts::{find_prompt, PROMPTS};
use super::resources::{find_resource, RESOURCES};
use super::scope::{require_scope, AgentToken, Scope, TokenStore};
use super::tools::{all_tools, find_tool, tools_schema_list};
use super::{agent_dir, load_config, AgentConfig, AgentCtx, BoxFuture, PORT_FILE, TOKENS_FILE};

/// Hard cap on a request body. Tool arguments and config imports are small;
/// this stops a loopback peer from making the process allocate unboundedly.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Fallback when the configured limit is zero. A rate limit that disables
/// itself on a zero value is a footgun, so zero means "use this".
pub const DEFAULT_RATE_LIMIT: u32 = 120;

/// Process-wide "is the layer switched on right now" flag.
///
/// The listener binds once and stays bound for the life of the process, but the
/// user must be able to switch the layer OFF from Settings without restarting
/// bzdium. Gating inside the request path gives that immediately, and it is the
/// safer direction: the moment the flag clears, every request is refused with
/// 503 before authentication, routing or a tool handler is reached. A stale
/// socket that still accepts TCP is then harmless.
///
/// Controlled by set_enabled; the flag lives here rather than in ServerState so
/// the settings command can flip it without holding the server's Arc.
static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Switches the live request gate on or off. Idempotent.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, std::sync::atomic::Ordering::SeqCst);
}

/// True when requests should currently be served.
pub fn is_enabled() -> bool {
    ENABLED.load(std::sync::atomic::Ordering::SeqCst)
}

/// The error a request sees when the layer is switched off mid-flight.
fn layer_disabled() -> AgentError {
    AgentError::new(
        ErrorCode::NotRunning,
        "The bzdium Agent Access layer is currently switched off.",
        "Enable 'Agent Access API' in bzdium Settings. The listener stays bound but serves nothing while it is off.",
    )
}

/// Path of the port file the stdio bridge and CLI clients read.
pub fn port_path(data_dir: &Path) -> PathBuf {
    agent_dir(data_dir).join(PORT_FILE)
}

/// Records the bound port so the stdio bridge and CLI clients can find it.
pub fn write_port(data_dir: &Path, port: u16) -> Result<(), String> {
    let dir = agent_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    crate::storage::atomic_write(&port_path(data_dir), &format!("{port}\n"))
        .map_err(|e| format!("cannot write the agent port file: {e}"))
}

/// Reads the port a running instance recorded, if any.
pub fn read_port(data_dir: &Path) -> Option<u16> {
    let text = std::fs::read_to_string(port_path(data_dir)).ok()?;
    text.trim().parse::<u16>().ok().filter(|p| *p != 0)
}

// ---------------------------------------------------------------------------
// Token store I/O and minting
// ---------------------------------------------------------------------------

pub fn tokens_path(data_dir: &Path) -> PathBuf {
    agent_dir(data_dir).join(TOKENS_FILE)
}

/// Loads agent/tokens.json. Missing or corrupt resolves to an empty store,
/// which authenticates nobody: the layer fails closed.
pub fn load_tokens(data_dir: &Path) -> TokenStore {
    match std::fs::read_to_string(tokens_path(data_dir)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            eprintln!("agent: ignoring corrupt {}: {e}", tokens_path(data_dir).display());
            TokenStore::default()
        }),
        Err(_) => TokenStore::default(),
    }
}

pub fn save_tokens(data_dir: &Path, store: &TokenStore) -> Result<(), String> {
    let dir = super::ensure_agent_dir(data_dir)?;
    let text = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    let path = dir.join(TOKENS_FILE);
    crate::storage::atomic_write(&path, &text)
        .map_err(|e| format!("cannot write the agent token file: {e}"))?;
    // Bearer keys at rest: owner-only on the platforms that have a umask.
    super::restrict_file(&path);
    Ok(())
}

/// Mints a fresh credential. Entropy comes from uuid v4 (CSPRNG-backed), and
/// two UUIDs are concatenated so a token carries 244 bits of randomness
/// rather than the 122 a single UUID provides.
pub fn mint_token(scope: Scope, label: &str) -> AgentToken {
    let raw = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    AgentToken {
        token: format!("bzd_agt_{raw}"),
        scope,
        label: label.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Security gate
// ---------------------------------------------------------------------------

/// The parts of a request the gate needs. Kept as plain borrowed data so the
/// gate is a pure function and can be tested exhaustively without a socket.
pub struct RequestFacts<'a> {
    pub method: &'a str,
    pub origin: Option<&'a str>,
    pub host: Option<&'a str>,
    pub authorization: Option<&'a str>,
    pub peer_is_loopback: bool,
}

/// A request that passed the gate.
#[derive(Debug, Clone)]
pub struct Authenticated {
    pub scope: Scope,
    pub actor: String,
}

/// True when the Host header names loopback and nothing else.
///
/// Accepts only: 127.0.0.1, 127.0.0.1:<port>, localhost, localhost:<port>.
/// The comparison is exact after lowercasing, so decimal/octal/hex IP
/// encodings (2130706433, 0x7f000001), suffixed names (localhost.evil.com)
/// and userinfo tricks (evil.com@localhost) are all rejected.
pub fn host_is_allowed(host: &str) -> bool {
    let host = host.trim();
    if host.is_empty() || host.contains('/') || host.contains('@') || host.contains(' ') {
        return false;
    }
    // Split an optional trailing port. More than one colon is not a plain
    // host:port pair and is rejected, which also covers IPv6 literals: this
    // server binds IPv4 loopback only, so an IPv6 Host cannot be us.
    let (name, port) = match host.rsplit_once(':') {
        Some((n, p)) => (n, Some(p)),
        None => (host, None),
    };
    if name.contains(':') {
        return false;
    }
    if let Some(p) = port {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        if p.parse::<u16>().is_err() {
            return false;
        }
    }
    let name = name.to_ascii_lowercase();
    name == "127.0.0.1" || name == "localhost"
}

/// Extracts the token from an Authorization header value.
/// Accepts only the Bearer scheme, case-insensitively.
pub fn bearer_token(header: &str) -> Option<&str> {
    let header = header.trim();
    let (scheme, rest) = header.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = rest.trim();
    if token.is_empty() {
        return None;
    }
    Some(token)
}

/// Per-token fixed-window limiter.
///
/// A fixed window is used deliberately: it is O(1) per request, cannot be
/// grown unboundedly by an attacker (expired entries are swept), and its
/// failure mode is a brief burst at a window boundary, which is acceptable
/// for a local single-user tool.
pub struct RateLimiter {
    limit: u32,
    window: Duration,
    entries: Mutex<HashMap<String, Window>>,
}

struct Window {
    started: Instant,
    count: u32,
}

impl RateLimiter {
    pub fn new(limit_per_minute: u32) -> Self {
        let limit = if limit_per_minute == 0 {
            DEFAULT_RATE_LIMIT
        } else {
            limit_per_minute
        };
        Self {
            limit,
            window: Duration::from_secs(60),
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn limit(&self) -> u32 {
        self.limit
    }

    /// Counts one request for key. On refusal returns seconds until the
    /// window rolls over, for the Retry-After header.
    pub fn check(&self, key: &str) -> Result<(), u64> {
        let now = Instant::now();
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        // Sweep closed windows so an attacker churning tokens cannot grow the
        // map without bound.
        if entries.len() > 64 {
            entries.retain(|_, w| now.duration_since(w.started) < self.window);
        }
        let window = entries
            .entry(key.to_string())
            .or_insert(Window { started: now, count: 0 });
        if now.duration_since(window.started) >= self.window {
            window.started = now;
            window.count = 0;
        }
        if window.count >= self.limit {
            let elapsed = now.duration_since(window.started).as_secs();
            let remaining = self.window.as_secs().saturating_sub(elapsed).max(1);
            return Err(remaining);
        }
        window.count += 1;
        Ok(())
    }
}

/// Where the gate gets its token set.
///
/// The live source re-reads agent/tokens.json on every request instead of
/// holding a snapshot. A snapshot would mean revocation is a lie: the Revoke
/// all keys button and revoke_agent_tokens delete the file's contents, and a
/// running listener that cached the old store would keep honouring every
/// deleted credential until the app restarted. For a control surface that can
/// delete services, "revoked" has to mean revoked now.
#[derive(Clone)]
pub enum TokenSource {
    /// Reads the token file named by this data dir on each request.
    File(PathBuf),
    /// A fixed set, for tests and embedders that supply their own store.
    Fixed(Arc<TokenStore>),
}

impl TokenSource {
    /// The current token set, failing closed if it cannot be read.
    pub fn current(&self) -> Arc<TokenStore> {
        match self {
            // load_tokens already degrades to an empty store on any read or
            // parse failure, so a deleted or corrupt file authenticates
            // nobody rather than falling back to something permissive.
            TokenSource::File(dir) => Arc::new(load_tokens(dir)),
            TokenSource::Fixed(store) => store.clone(),
        }
    }
}

/// Whether admin-scope tools may be used right now, re-read per request when
/// the guard follows a data dir.
///
/// allow_admin_scope is the operator's ceiling. Checking it only while minting
/// a token made it a one-time gate: an admin key minted while the box was
/// ticked kept deleting services after the box was unticked, so "Allow admin
/// tools" could be switched off without reducing anything. The ceiling has to
/// be consulted when the request is authorised.
#[derive(Clone)]
pub enum AdminCeiling {
    /// Not binding: the guard was built from an explicit config.
    Unmanaged,
    /// Read from this data dir on each request.
    File(PathBuf),
}

impl AdminCeiling {
    fn allows(&self) -> bool {
        match self {
            // An unreadable config resolves to the default, which has admin
            // off, so a corrupt file closes the ceiling rather than opening it.
            AdminCeiling::File(dir) => load_config(dir).allow_admin_scope,
            AdminCeiling::Unmanaged => true,
        }
    }
}

/// Everything the gate needs besides the request itself.
pub struct Guard {
    tokens: TokenSource,
    admin: AdminCeiling,
    pub limiter: RateLimiter,
}

impl Guard {
    pub fn new(tokens: Arc<TokenStore>, config: &AgentConfig) -> Self {
        Self {
            tokens: TokenSource::Fixed(tokens),
            admin: AdminCeiling::Unmanaged,
            limiter: RateLimiter::new(config.rate_limit_per_minute),
        }
    }

    /// Builds a guard whose authorisation follows the token file, so revoking
    /// a key takes effect on the next request rather than the next restart,
    /// and whose admin ceiling follows agent/config.json.
    pub fn following_file(data_dir: PathBuf, config: &AgentConfig) -> Self {
        Self {
            tokens: TokenSource::File(data_dir.clone()),
            admin: AdminCeiling::File(data_dir),
            limiter: RateLimiter::new(config.rate_limit_per_minute),
        }
    }

    /// Runs every security control in order and returns the caller identity.
    ///
    /// Order matters: cheap, unauthenticated rejections happen first, so a
    /// probe cannot spend body-read or tool-dispatch work. A bad token also
    /// never consumes rate-limit budget, so a hostile scanner cannot lock the
    /// legitimate token out of its own quota.
    pub fn evaluate(&self, facts: &RequestFacts<'_>) -> Result<Authenticated, AgentError> {
        // 1. Loopback peer only. Defence in depth: the socket is already bound
        //    to 127.0.0.1, so a non-loopback peer would mean something is very
        //    wrong.
        if !facts.peer_is_loopback {
            return Err(forbidden(
                "This endpoint only accepts connections from the local machine.",
                "Connect to 127.0.0.1 from a process running on this machine.",
            ));
        }

        // 2. Never answer a CORS preflight. 405, no CORS headers: the browser
        //    then refuses to send the real request, so a hostile page never
        //    gets to present a token at all.
        if facts.method.eq_ignore_ascii_case("OPTIONS") {
            return Err(method_not_allowed(
                "CORS preflight is not supported and will not be answered.",
                "This API is for local native clients. Send the real request with an Authorization: Bearer header; do not call it from browser JavaScript.",
            ));
        }

        // 3. Any Origin header means a browser initiated this request.
        if facts.origin.is_some() {
            return Err(forbidden(
                "Requests carrying an Origin header are rejected: this API is not reachable from browser contexts.",
                "Call this API from a local process (CLI, editor, agent runtime), not from web page JavaScript.",
            ));
        }

        // 4. Host validation, which is what defeats DNS rebinding.
        let host = facts.host.unwrap_or("");
        if !host_is_allowed(host) {
            return Err(forbidden(
                "The Host header is missing or is not a loopback host.",
                "Send Host: 127.0.0.1:<port> (or localhost:<port>). This check exists to defeat DNS rebinding.",
            ));
        }

        // 5. Bearer authentication, constant-time inside authenticate().
        let presented = facts.authorization.and_then(bearer_token).ok_or_else(|| {
            unauthorized(
                "A bearer token is required.",
                "Send Authorization: Bearer bzd_agt_... . Mint a token in bzdium's Agent Access settings.",
            )
        })?;
        // Read the token set now, not at startup, so a revoked credential is
        // refused immediately. The read is small and only happens for a
        // request that already passed the loopback, Origin and Host checks.
        let tokens = self.tokens.current();
        let token = tokens.authenticate(presented).ok_or_else(|| {
            unauthorized(
                "The bearer token is not recognised.",
                "Check that the token was copied in full and belongs to the currently running bzdium instance. Mint a new token if it was rotated.",
            )
        })?;

        // 6. Per-token rate limiting, applied after identity is established.
        if let Err(retry_after) = self.limiter.check(&token.token) {
            return Err(AgentError::new(
                ErrorCode::RateLimited,
                format!(
                    "Rate limit exceeded: at most {} requests per minute per token.",
                    self.limiter.limit()
                ),
                "Wait for the Retry-After interval, then retry. Batch reads (GET /manifest) instead of polling single tools.",
            )
            .with_details(json!({
                "retryAfterSecs": retry_after,
                "limitPerMinute": self.limiter.limit()
            })));
        }

        // 7. The admin ceiling. An admin token is only admin while the
        //    operator's ceiling is open; unticking the box has to reduce the
        //    power of keys that already exist, not merely stop new ones.
        let scope = if token.scope == Scope::Admin && !self.admin.allows() {
            return Err(AgentError::new(
                ErrorCode::ForbiddenScope,
                "This token is admin-scoped, but admin tools are currently disabled.",
                "Tick 'Allow admin-scope tools' in bzdium's Agent Access settings to use this key, or use a write-scoped token.",
            )
            .with_details(json!({
                "grantedScope": "admin",
                "requiredScope": "admin",
                "reason": "admin ceiling closed"
            })));
        } else {
            token.scope
        };

        let actor = if token.label.trim().is_empty() {
            format!("{scope}#token")
        } else {
            token.label.clone()
        };
        Ok(Authenticated { scope, actor })
    }
}

fn unauthorized(message: &str, suggestion: &str) -> AgentError {
    AgentError::new(ErrorCode::Unauthorized, message, suggestion)
}

/// A refusal that happens BEFORE identity is established.
///
/// These are 403, not 401, and the distinction is deliberate. The presented
/// credentials may be perfectly valid: the request is refused because of how it
/// arrived -- a browser Origin, a rebound Host, a non-loopback peer. Replying
/// 401 would be dishonest, and worse, error_response attaches
/// "WWW-Authenticate: Bearer" to every 401, which actively invites the caller
/// to retry with credentials. A hostile page must never be told that supplying
/// a token would help. 403 says "understood, and refused".
///
/// AgentError/ErrorCode are a fixed contract with no dedicated variant for
/// request hygiene, so FORBIDDEN_SCOPE carries these; the message and
/// suggestion name the real cause.
fn forbidden(message: &str, suggestion: &str) -> AgentError {
    AgentError::new(ErrorCode::ForbiddenScope, message, suggestion)
}

fn method_not_allowed(message: &str, suggestion: &str) -> AgentError {
    AgentError::new(ErrorCode::MethodNotAllowed, message, suggestion)
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

/// A resolved route. Path segments are already percent-decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Health,
    Manifest,
    OpenApi,
    ListTools,
    CallTool(String),
    ListResources,
    /// The resource path with the leading slash stripped, e.g. services/abc.
    ReadResource(String),
    ListPrompts,
    GetPrompt(String),
    Mcp,
}

impl Route {
    /// The only HTTP method this route accepts.
    pub fn method(&self) -> Method {
        match self {
            Route::CallTool(_) | Route::Mcp => Method::POST,
            _ => Method::GET,
        }
    }
}

/// Maps a request path onto a route. Returns None for anything unrecognised,
/// so the caller answers 404 rather than guessing.
///
/// @BT@/api/v1/agent/tools/add_service@BT@ is a per-tool concrete path, exactly what the
/// OpenAPI document advertises, and it resolves to the same CallTool route as
/// @BT@/api/v1/agent/tools/{name}@BT@: in both forms the segment after /tools/ is a
/// tool name, so there is no ambiguity to resolve.
pub fn resolve_route(path: &str) -> Option<Route> {
    let path = path.trim_end_matches('/');
    let rest = path.strip_prefix(API_PREFIX)?;
    if rest.is_empty() {
        return None;
    }

    match rest {
        "/health" => return Some(Route::Health),
        "/manifest" => return Some(Route::Manifest),
        "/openapi.json" => return Some(Route::OpenApi),
        "/tools" => return Some(Route::ListTools),
        "/resources" => return Some(Route::ListResources),
        "/prompts" => return Some(Route::ListPrompts),
        MCP_PATH => return Some(Route::Mcp),
        _ => {}
    }
    if let Some(name) = rest.strip_prefix("/tools/") {
        if name.is_empty() || name.contains('/') {
            return None;
        }
        return Some(Route::CallTool(percent_decode(name)?));
    }
    if let Some(name) = rest.strip_prefix("/prompts/") {
        if name.is_empty() || name.contains('/') {
            return None;
        }
        return Some(Route::GetPrompt(percent_decode(name)?));
    }
    if let Some(rest) = rest.strip_prefix("/resources/") {
        if rest.is_empty() {
            return None;
        }
        return Some(Route::ReadResource(percent_decode(rest)?));
    }
    None
}

/// Maps a REST resource path fragment back to its bzdium:// URI.
/// services -> bzdium://services, services/abc -> bzdium://services/abc.
pub fn resource_uri(fragment: &str) -> String {
    format!("bzdium://{}", fragment.trim_start_matches('/'))
}

/// Minimal percent-decoding, no dependency. Returns None on a malformed
/// escape rather than silently mangling the value.
pub fn percent_decode(input: &str) -> Option<String> {
    if !input.contains('%') {
        return Some(input.to_string());
    }
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            let value = u8::from_str_radix(hex, 16).ok()?;
            // A decoded NUL could truncate a path downstream; refuse it.
            if value == 0 {
                return None;
            }
            out.push(value);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Parses a query string into a decoded map. Later keys win, which matches
/// how every HTTP library behaves.
pub fn parse_query(query: Option<&str>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(query) = query else { return out };
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        if let (Some(k), Some(v)) = (percent_decode(k), percent_decode(v)) {
            out.insert(k, v);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Backend seam
// ---------------------------------------------------------------------------

/// What a tool call needs to know about its caller.
pub struct ToolCallRequest {
    /// "rest", "mcp" or "stdio"; recorded in the audit log.
    pub transport: &'static str,
    pub actor: String,
    pub scope: Scope,
    pub name: String,
    pub args: Value,
    pub dry_run: bool,
}

/// The registry-facing half of the transport.
///
/// This trait exists so the transport can be exercised end to end over a real
/// socket in tests, without a Tauri AppHandle. It is the only seam in the
/// layer: everything above it is route and security logic, everything below it
/// is the registry the UI also uses.
pub trait AgentBackend: Send + Sync + 'static {
    fn health(&self) -> Value;
    fn manifest(&self) -> Value;
    fn openapi_json(&self) -> String;
    fn list_tools(&self) -> Value;
    fn list_resources(&self) -> Value;
    fn list_prompts(&self) -> Value;
    fn call_tool(&self, req: ToolCallRequest) -> BoxFuture<'static, Result<Value, AgentError>>;
    fn read_resource(
        &self,
        transport: &'static str,
        actor: String,
        scope: Scope,
        uri: String,
    ) -> BoxFuture<'static, Result<(String, String), AgentError>>;
    fn render_prompt(
        &self,
        transport: &'static str,
        actor: String,
        scope: Scope,
        name: String,
        args: HashMap<String, String>,
    ) -> Result<Value, AgentError>;
    fn mcp(&self, scope: Scope, actor: String, body: Value) -> BoxFuture<'static, Result<Value, AgentError>>;
}

/// The production backend: delegates to the real registry, so a REST call and
/// a UI click reach the exact same Rust function.
pub struct RegistryBackend {
    pub ctx: Arc<AgentCtx>,
    pub config: AgentConfig,
}

impl RegistryBackend {
    fn audit_entry(
        transport: &'static str,
        action: &str,
        actor: &str,
        scope: Scope,
        outcome: &str,
        error: Option<&AgentError>,
        elapsed: Duration,
    ) -> AuditEntry {
        AuditEntry {
            ts: now_unix(),
            action: action.to_string(),
            transport: transport.to_string(),
            actor: actor.to_string(),
            scope: scope.as_str().to_string(),
            outcome: outcome.to_string(),
            duration_ms: Some(elapsed.as_millis() as u64),
            dry_run: None,
            error_code: error.map(|e| e.code.to_string()),
            args: None,
        }
    }

    fn health_value(&self) -> Value {
        json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
            "uptimeSecs": self.ctx.uptime_secs(),
            "transport": "rest, mcp-http",
            "agentEnabled": self.config.enabled,
        })
    }
}

impl AgentBackend for RegistryBackend {
    fn health(&self) -> Value {
        self.health_value()
    }

    fn manifest(&self) -> Value {
        let resources: Vec<Value> = RESOURCES
            .iter()
            .map(|r| {
                json!({
                    "uri": r.uri_template,
                    "name": r.name,
                    "description": r.description,
                    "mimeType": r.mime_type,
                    "scope": r.scope.as_str(),
                    "restPath": openapi::resource_path_template(r.uri_template),
                })
            })
            .collect();
        let prompts: Vec<Value> = PROMPTS
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "description": p.description,
                    "scope": p.scope.as_str(),
                    "arguments": (p.arguments)(),
                })
            })
            .collect();
        let tools = tools_schema_list();
        json!({
            "name": "bzdium",
            "version": env!("CARGO_PKG_VERSION"),
            "apiVersion": "v1",
            "basePath": API_PREFIX,
            "counts": {
                "tools": all_tools().len(),
                "resources": resources.len(),
                "prompts": prompts.len(),
            },
            "tools": tools,
            "resources": resources,
            "prompts": prompts,
        })
    }

    fn openapi_json(&self) -> String {
        openapi::document_json()
    }

    fn list_tools(&self) -> Value {
        let tools = tools_schema_list();
        let count = tools.as_array().map(|a| a.len()).unwrap_or(0);
        json!({ "count": count, "tools": tools })
    }

    fn list_resources(&self) -> Value {
        let resources: Vec<Value> = RESOURCES
            .iter()
            .map(|r| {
                json!({
                    "uri": r.uri_template,
                    "name": r.name,
                    "description": r.description,
                    "mimeType": r.mime_type,
                    "scope": r.scope.as_str(),
                    "restPath": openapi::resource_path_template(r.uri_template),
                })
            })
            .collect();
        json!({ "count": resources.len(), "resources": resources })
    }

    fn list_prompts(&self) -> Value {
        let prompts: Vec<Value> = PROMPTS
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "description": p.description,
                    "scope": p.scope.as_str(),
                    "arguments": (p.arguments)(),
                })
            })
            .collect();
        json!({ "count": prompts.len(), "prompts": prompts })
    }

    fn call_tool(&self, req: ToolCallRequest) -> BoxFuture<'static, Result<Value, AgentError>> {
        let ctx = self.ctx.clone();
        let data_dir = self.ctx.data_dir.clone();
        Box::pin(async move {
            let started = Instant::now();
            // Redact BEFORE dispatch: this must never be able to fail after a
            // mutation has executed (the audit write below is the only record
            // of what an agent did).
            let redacted_args = redact(&req.args);
            let result = dispatch(&ctx, &req.name, req.args.clone(), req.dry_run).await;
            let elapsed = started.elapsed();
            let mut entry = RegistryBackend::audit_entry(
                req.transport,
                &req.name,
                &req.actor,
                req.scope,
                "ok",
                None,
                elapsed,
            );
            entry.args = Some(redacted_args);
            match result {
                Ok(dispatched) => {
                    entry.dry_run = Some(dispatched.dry_run);
                    record(&data_dir, &entry);
                    Ok(dispatched.value)
                }
                Err(err) => {
                    entry.outcome = "error".into();
                    entry.dry_run = Some(req.dry_run);
                    entry.error_code = Some(err.code.to_string());
                    record(&data_dir, &entry);
                    Err(err)
                }
            }
        })
    }

    fn read_resource(
        &self,
        transport: &'static str,
        actor: String,
        scope: Scope,
        uri: String,
    ) -> BoxFuture<'static, Result<(String, String), AgentError>> {
        let ctx = self.ctx.clone();
        let data_dir = self.ctx.data_dir.clone();
        Box::pin(async move {
            let started = Instant::now();
            let Some((spec, params)) = find_resource(&uri) else {
                return Err(AgentError::new(
                    ErrorCode::UnknownResource,
                    format!("No resource matches '{uri}'."),
                    "Call GET /api/v1/agent/resources (or MCP resources/list) for the valid URI templates.",
                )
                .with_details(json!({ "uri": uri })));
            };
            let outcome = (spec.render)(&ctx, &params).await;
            let elapsed = started.elapsed();
            match outcome {
                Ok(text) => {
                    record(
                        &data_dir,
                        &RegistryBackend::audit_entry(transport, &uri, &actor, scope, "ok", None, elapsed),
                    );
                    Ok((text, spec.mime_type.to_string()))
                }
                Err(err) => {
                    record(
                        &data_dir,
                        &RegistryBackend::audit_entry(
                            transport, &uri, &actor, scope, "error", Some(&err), elapsed,
                        ),
                    );
                    Err(err)
                }
            }
        })
    }

    fn render_prompt(
        &self,
        transport: &'static str,
        actor: String,
        scope: Scope,
        name: String,
        args: HashMap<String, String>,
    ) -> Result<Value, AgentError> {
        let started = Instant::now();
        let Some(spec) = find_prompt(&name) else {
            return Err(AgentError::new(
                ErrorCode::UnknownPrompt,
                format!("No prompt named '{name}' is registered."),
                "Call GET /api/v1/agent/prompts (or MCP prompts/list) for the valid prompt names.",
            )
            .with_details(json!({ "prompt": name })));
        };
        let text = (spec.render)(&args);
        record(
            &self.ctx.data_dir,
            &RegistryBackend::audit_entry(
                transport,
                &format!("prompt:{}", spec.name),
                &actor,
                scope,
                "ok",
                None,
                started.elapsed(),
            ),
        );
        Ok(json!({
            "name": spec.name,
            "description": spec.description,
            "text": text,
        }))
    }

    fn mcp(&self, scope: Scope, actor: String, body: Value) -> BoxFuture<'static, Result<Value, AgentError>> {
        let ctx = self.ctx.clone();
        Box::pin(async move { Ok(mcp::handle(ctx, scope, actor, body).await) })
    }
}

// ---------------------------------------------------------------------------
// Server state and responses
// ---------------------------------------------------------------------------

pub struct ServerState {
    pub backend: Arc<dyn AgentBackend>,
    pub guard: Guard,
}

fn header_str<'a>(headers: &'a hyper::HeaderMap, name: hyper::header::HeaderName) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn base_response(status: StatusCode, body: Bytes) -> Response<Full<Bytes>> {
    let len = body.len();
    let mut resp = Response::new(Full::new(body));
    *resp.status_mut() = status;
    let headers = resp.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("application/json; charset=utf-8"),
    );
    headers.insert(
        hyper::header::HeaderName::from_static("cache-control"),
        hyper::header::HeaderValue::from_static("no-store"),
    );
    headers.insert(
        hyper::header::HeaderName::from_static("x-content-type-options"),
        hyper::header::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        hyper::header::HeaderName::from_static("referrer-policy"),
        hyper::header::HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        hyper::header::CONTENT_LENGTH,
        hyper::header::HeaderValue::from_str(&len.to_string())
            .unwrap_or_else(|_| hyper::header::HeaderValue::from_static("0")),
    );
    // Deliberately no Access-Control-* header, ever: the browser must never
    // see this endpoint as readable, including on the error path.
    resp
}

pub fn json_response(status: u16, body: &Value) -> Response<Full<Bytes>> {
    let text = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    base_response(
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Bytes::from(text),
    )
}

fn with_header(
    mut resp: Response<Full<Bytes>>,
    name: hyper::header::HeaderName,
    value: String,
) -> Response<Full<Bytes>> {
    if let Ok(v) = hyper::header::HeaderValue::from_str(&value) {
        resp.headers_mut().insert(name, v);
    }
    resp
}

/// Renders an AgentError with the status its code implies, plus the headers a
/// correct client needs (WWW-Authenticate on 401, Retry-After on 429).
pub fn error_response(err: &AgentError) -> Response<Full<Bytes>> {
    let status = err.status();
    let body = serde_json::to_value(err).unwrap_or_else(|_| {
        json!({
            "code": "INTERNAL_ERROR",
            "message": "unserialisable error",
            "suggestion": "Report this as a bzdium bug."
        })
    });
    let mut resp = json_response(status, &body);
    if status == 401 {
        resp = with_header(resp, WWW_AUTHENTICATE, "Bearer realm=\"bzdium-agent\"".to_string());
    }
    if status == 429 {
        let retry = err
            .details
            .as_ref()
            .and_then(|d| d.get("retryAfterSecs"))
            .and_then(Value::as_u64)
            .unwrap_or(60);
        resp = with_header(resp, RETRY_AFTER, retry.to_string());
    }
    resp
}

fn method_response(route: &Route) -> Response<Full<Bytes>> {
    let allowed = route.method();
    let err = AgentError::new(
        ErrorCode::MethodNotAllowed,
        format!("This route accepts {allowed} only."),
        format!("Re-send the request with {allowed}."),
    );
    with_header(error_response(&err), ALLOW, allowed.to_string())
}

fn not_found(path: &str) -> Response<Full<Bytes>> {
    let err = AgentError::new(
        ErrorCode::NotFound,
        format!("No agent route matches '{path}'."),
        format!(
            "Routes are mounted under {API_PREFIX}. Start with GET {API_PREFIX}/manifest, or fetch {API_PREFIX}/openapi.json."
        ),
    );
    error_response(&err)
}

fn payload_too_large() -> AgentError {
    AgentError::new(
        ErrorCode::ConfigTooLarge,
        format!("The request body exceeds the {MAX_BODY_BYTES} byte limit."),
        "Reduce the payload. Tool arguments are small; import_config documents are capped at 2 MiB and should be sent to the desktop UI instead.",
    )
}

fn scope_error(granted: Scope, required: Scope, what: &str) -> AgentError {
    AgentError::new(
        ErrorCode::ForbiddenScope,
        format!("This token has scope '{granted}' but '{what}' requires '{required}'."),
        format!("Use a token minted with scope '{required}'."),
    )
    .with_details(json!({
        "grantedScope": granted.as_str(),
        "requiredScope": required.as_str(),
        "target": what,
    }))
}

fn raw_response(status: StatusCode, body: String, mime: &str) -> Response<Full<Bytes>> {
    let resp = base_response(status, Bytes::from(body));
    with_header(resp, CONTENT_TYPE, mime.to_string())
}

fn with_status(mut resp: Response<Full<Bytes>>, status: StatusCode) -> Response<Full<Bytes>> {
    *resp.status_mut() = status;
    resp
}

fn require_read(
    scope: Scope,
    what: &str,
    render: impl FnOnce() -> Response<Full<Bytes>>,
) -> Response<Full<Bytes>> {
    if scope.satisfies(Scope::Read) {
        render()
    } else {
        error_response(&scope_error(scope, Scope::Read, what))
    }
}

// ---------------------------------------------------------------------------
// Request handling
// ---------------------------------------------------------------------------

/// Handles one request. Every branch is deliberate; read the gate first.
pub async fn handle_request(
    state: &Arc<ServerState>,
    req: Request<Incoming>,
    peer_is_loopback: bool,
) -> Response<Full<Bytes>> {
    let (parts, body) = req.into_parts();
    let method = parts.method.clone();
    let path = parts.uri.path().to_string();
    let query = parts.uri.query().map(str::to_string);

    // --- Switch: honour a live disable before doing anything else. -------
    // Checked first so a switched-off layer reveals nothing: not a route list,
    // not a token check, not a tool. 503 rather than 403 because this is a
    // transient operator choice, not a refusal of the caller's identity.
    if !is_enabled() {
        let mut resp = error_response(&layer_disabled());
        *resp.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
        return resp;
    }

    // --- Gate: everything below this point is authenticated. -------------
    let facts = RequestFacts {
        method: method.as_str(),
        origin: header_str(&parts.headers, ORIGIN),
        host: header_str(&parts.headers, HOST),
        authorization: header_str(&parts.headers, AUTHORIZATION),
        peer_is_loopback,
    };
    let authed = match state.guard.evaluate(&facts) {
        Ok(a) => a,
        // A rejected request must never reach routing or a tool handler.
        Err(err) => return error_response(&err),
    };

    // --- Route ----------------------------------------------------------
    let Some(route) = resolve_route(&path) else {
        return not_found(&path);
    };
    if method != route.method() {
        return method_response(&route);
    }

    // --- Body (read only after the caller is authenticated) -------------
    let needs_body = matches!(route, Route::CallTool(_) | Route::Mcp);
    let body_bytes: Bytes = if needs_body {
        match Limited::new(body, MAX_BODY_BYTES).collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(_) => {
                return with_status(error_response(&payload_too_large()), StatusCode::PAYLOAD_TOO_LARGE)
            }
        }
    } else {
        Bytes::new()
    };

    let scope = authed.scope;
    let actor = authed.actor;

    match route {
        Route::Health => require_read(scope, "health", || json_response(200, &state.backend.health())),
        Route::Manifest => require_read(scope, "manifest", || json_response(200, &state.backend.manifest())),
        Route::OpenApi => require_read(scope, "openapi.json", || {
            let doc = state.backend.openapi_json();
            raw_response(StatusCode::OK, doc, "application/json; charset=utf-8")
        }),
        Route::ListTools => require_read(scope, "tools", || json_response(200, &state.backend.list_tools())),
        Route::ListResources => {
            require_read(scope, "resources", || json_response(200, &state.backend.list_resources()))
        }
        Route::ListPrompts => require_read(scope, "prompts", || json_response(200, &state.backend.list_prompts())),

        Route::CallTool(name) => {
            let Some(spec) = find_tool(&name) else {
                return error_response(&super::unknown_tool(&name));
            };
            if let Err(err) = require_scope(scope, spec.scope, spec.name) {
                return error_response(&err);
            }
            let mut args: Value = if body_bytes.is_empty() {
                json!({})
            } else {
                match serde_json::from_slice::<Value>(&body_bytes) {
                    Ok(v) => v,
                    Err(e) => {
                        return error_response(&AgentError::invalid_parameter(
                            "body",
                            format!("The request body is not valid JSON: {e}"),
                            "Send a JSON object of tool arguments. Call GET /tools for each tool's inputSchema.",
                        ))
                    }
                }
            };
            if args.is_null() {
                args = json!({});
            }
            // The dryRun query override lets a client preview without
            // rewriting the body; an explicit body flag still wins.
            let dry_run = super::guard::wants_dry_run(&args)
                || parse_query(query.as_deref())
                    .get("dryRun")
                    .is_some_and(|v| v == "true" || v == "1");
            let args = super::guard::strip_dry_run(args);
            match state
                .backend
                .call_tool(ToolCallRequest {
                    transport: "rest",
                    actor,
                    scope,
                    name,
                    args,
                    dry_run,
                })
                .await
            {
                Ok(value) => json_response(200, &value),
                Err(err) => error_response(&err),
            }
        }

        Route::ReadResource(fragment) => {
            let uri = resource_uri(&fragment);
            let Some((spec, _)) = find_resource(&uri) else {
                return error_response(
                    &AgentError::new(
                        ErrorCode::UnknownResource,
                        format!("No resource matches '{uri}'."),
                        "Call GET /api/v1/agent/resources for the valid URI templates.",
                    )
                    .with_details(json!({ "uri": uri })),
                );
            };
            if let Err(err) = require_scope(scope, spec.scope, spec.name) {
                return error_response(&err);
            }
            match state.backend.read_resource("rest", actor, scope, uri).await {
                Ok((text, mime)) => raw_response(StatusCode::OK, text, &mime),
                Err(err) => error_response(&err),
            }
        }

        Route::GetPrompt(name) => {
            let Some(spec) = find_prompt(&name) else {
                return error_response(
                    &AgentError::new(
                        ErrorCode::UnknownPrompt,
                        format!("No prompt named '{name}' is registered."),
                        "Call GET /api/v1/agent/prompts for the valid prompt names.",
                    )
                    .with_details(json!({ "prompt": name })),
                );
            };
            if let Err(err) = require_scope(scope, spec.scope, spec.name) {
                return error_response(&err);
            }
            let args = parse_query(query.as_deref());
            match state.backend.render_prompt("rest", actor, scope, name, args) {
                Ok(value) => json_response(200, &value),
                Err(err) => error_response(&err),
            }
        }

        Route::Mcp => {
            let payload: Value = if body_bytes.is_empty() {
                return error_response(&AgentError::invalid_parameter(
                    "body",
                    "The MCP endpoint requires a JSON-RPC request body.",
                    "POST a JSON-RPC 2.0 object, e.g. {\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}.",
                ));
            } else {
                match serde_json::from_slice::<Value>(&body_bytes) {
                    Ok(v) => v,
                    Err(e) => {
                        return error_response(&AgentError::invalid_parameter(
                            "body",
                            format!("The request body is not valid JSON: {e}"),
                            "Send a JSON-RPC 2.0 request object.",
                        ))
                    }
                }
            };
            match state.backend.mcp(scope, actor, payload).await {
                // A notification (no id) produces no response object.
                Ok(value) if value.is_null() => {
                    raw_response(StatusCode::ACCEPTED, String::new(), "application/json; charset=utf-8")
                }
                Ok(value) => json_response(200, &value),
                Err(err) => error_response(&err),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Server lifecycle
// ---------------------------------------------------------------------------

/// Builds the production state from the agent context and config.
pub fn build_state(ctx: Arc<AgentCtx>, config: &AgentConfig) -> Arc<ServerState> {
    Arc::new(ServerState {
        backend: Arc::new(RegistryBackend {
            ctx: ctx.clone(),
            config: config.clone(),
        }),
        // Follows the token file rather than snapshotting it, so revoking a
        // key takes effect on the next request.
        guard: Guard::following_file(ctx.data_dir.clone(), config),
    })
}

/// Binds 127.0.0.1, records the port, then serves until the process ends.
///
/// The returned port is available as soon as the socket is bound; this future
/// itself stays pending because the accept loop lives in a spawned task that
/// keeps the runtime alive.
pub async fn serve(ctx: Arc<AgentCtx>, config: AgentConfig) -> Result<u16, String> {
    let data_dir = ctx.data_dir.clone();
    let state = build_state(ctx, &config);
    serve_with_state(state, data_dir, config.port).await
}

/// The testable core of serve(): bind, publish the port, then accept forever.
pub async fn serve_with_state(
    state: Arc<ServerState>,
    data_dir: PathBuf,
    requested_port: u16,
) -> Result<u16, String> {
    // 127.0.0.1 only. Binding 0.0.0.0 here would publish a control API that
    // drives a browser-hosting process to the whole LAN.
    let listener = TcpListener::bind((IpAddr::from([127, 0, 0, 1]), requested_port))
        .await
        .map_err(|e| format!("cannot bind 127.0.0.1:{requested_port}: {e}"))?;
    let addr = listener
        .local_addr()
        .map_err(|e| format!("cannot read the bound address: {e}"))?;
    write_port(&data_dir, addr.port())?;
    tokio::spawn(async move {
        accept_loop(listener, state).await;
    });
    Ok(addr.port())
}

/// Spawns the server on its own multi-threaded runtime in a background thread
/// and returns the bound port once the socket is listening.
///
/// This is the entry point for callers that are not already inside a runtime,
/// which is the normal case from Tauri's setup hook.
pub fn spawn(ctx: Arc<AgentCtx>, config: AgentConfig) -> Result<u16, String> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<u16, String>>();
    std::thread::Builder::new()
        .name("bzdium-agent-http".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(format!("cannot start the agent runtime: {e}")));
                    return;
                }
            };
            runtime.block_on(async move {
                match serve(ctx, config).await {
                    Ok(port) => {
                        let _ = tx.send(Ok(port));
                        // Keep this runtime, and therefore the accept loop,
                        // alive for the lifetime of the process.
                        std::future::pending::<()>().await;
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                    }
                }
            });
        })
        .map_err(|e| format!("cannot start the agent thread: {e}"))?;
    rx.recv_timeout(Duration::from_secs(10))
        .map_err(|_| "timed out waiting for the agent listener to bind".to_string())?
}

/// Accepts connections forever. Loopback is re-checked per connection so a
/// future bind change cannot silently widen the surface.
pub async fn accept_loop(listener: TcpListener, state: Arc<ServerState>) {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                // A transient accept failure must not kill the listener.
                eprintln!("agent http: accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let peer_is_loopback = is_loopback_peer(&peer);
        let state = state.clone();
        tokio::spawn(async move {
            serve_connection(stream, peer_is_loopback, state).await;
        });
    }
}

pub fn is_loopback_peer(addr: &SocketAddr) -> bool {
    addr.ip().is_loopback()
}

async fn serve_connection(stream: TcpStream, peer_is_loopback: bool, state: Arc<ServerState>) {
    let io = TokioIo::new(stream);
    let service = service_fn(move |req: Request<Incoming>| {
        let state = state.clone();
        async move { Ok::<_, std::convert::Infallible>(handle_request(&state, req, peer_is_loopback).await) }
    });
    // http1 only: no h2 upgrade path, so no h2c smuggling surface.
    if let Err(e) = hyper::server::conn::http1::Builder::new()
        .keep_alive(true)
        .serve_connection(io, service)
        .await
    {
        // Client disconnects are routine and must not be logged as errors.
        let msg = e.to_string();
        if !msg.contains("connection closed") && !msg.contains("broken pipe") {
            eprintln!("agent http: connection error: {msg}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------- Host validation (DNS rebinding) ----------------

    #[test]
    fn loopback_hosts_are_accepted() {
        for host in [
            "127.0.0.1",
            "127.0.0.1:47821",
            "localhost",
            "localhost:1",
            "LOCALHOST:8080",
            " 127.0.0.1:9000 ",
        ] {
            assert!(host_is_allowed(host), "should accept {host:?}");
        }
    }

    #[test]
    fn rebinding_and_ssrf_hosts_are_rejected() {
        for host in [
            "",
            " ",
            "evil.com",
            "localhost.evil.com",
            "127.0.0.1.evil.com",
            "evil.com:127.0.0.1",
            "127.0.0.1:80:90",
            "2130706433",
            "0x7f000001",
            "017700000001",
            "127.0.0.1@evil.com",
            "evil.com@127.0.0.1",
            "127.0.0.1:abc",
            "127.0.0.1:",
            "[::1]",
            "[::1]:8080",
            "::1",
            "0.0.0.0",
            "10.0.0.5",
            "localhost/evil",
            "local host",
        ] {
            assert!(!host_is_allowed(host), "should reject {host:?}");
        }
    }

    // ---------------- Bearer parsing ----------------

    #[test]
    fn bearer_scheme_is_parsed_case_insensitively() {
        assert_eq!(bearer_token("Bearer bzd_agt_abc"), Some("bzd_agt_abc"));
        assert_eq!(bearer_token("bearer bzd_agt_abc"), Some("bzd_agt_abc"));
        assert_eq!(bearer_token("BEARER   bzd_agt_abc  "), Some("bzd_agt_abc"));
    }

    #[test]
    fn non_bearer_and_malformed_authorization_is_rejected() {
        for header in ["", "bzd_agt_abc", "Basic dXNlcjpwYXNz", "Bearer", "Bearer ", "Token bzd_agt_x"] {
            assert_eq!(bearer_token(header), None, "should reject {header:?}");
        }
    }

    // ---------------- Rate limiting ----------------

    #[test]
    fn rate_limiter_allows_up_to_the_limit_then_refuses() {
        let limiter = RateLimiter::new(3);
        for i in 0..3 {
            assert!(limiter.check("tok").is_ok(), "request {i} should pass");
        }
        let err = limiter.check("tok").expect_err("4th request must be refused");
        assert!((1..=60).contains(&err), "retry-after must be sane, got {err}");
    }

    #[test]
    fn rate_limiter_is_per_token() {
        let limiter = RateLimiter::new(1);
        assert!(limiter.check("a").is_ok());
        assert!(limiter.check("a").is_err());
        assert!(limiter.check("b").is_ok(), "a second token has its own budget");
    }

    #[test]
    fn zero_configured_limit_falls_back_to_the_default() {
        assert_eq!(RateLimiter::new(0).limit(), DEFAULT_RATE_LIMIT);
    }

    // ---------------- Gate ordering ----------------

    fn guard_with(tokens: &[(&str, Scope)], limit: u32) -> Guard {
        let store = TokenStore {
            tokens: tokens
                .iter()
                .map(|(t, s)| AgentToken {
                    token: (*t).to_string(),
                    scope: *s,
                    label: "test".into(),
                })
                .collect(),
        };
        Guard::new(
            Arc::new(store),
            &AgentConfig {
                rate_limit_per_minute: limit,
                ..Default::default()
            },
        )
    }

    fn facts<'a>(method: &'a str, authorization: Option<&'a str>) -> RequestFacts<'a> {
        RequestFacts {
            method,
            origin: None,
            host: Some("127.0.0.1:47821"),
            authorization,
            peer_is_loopback: true,
        }
    }

    #[test]
    fn gate_accepts_a_well_formed_authenticated_request() {
        let guard = guard_with(&[("bzd_agt_ok", Scope::Read)], 100);
        let auth = guard
            .evaluate(&facts("GET", Some("Bearer bzd_agt_ok")))
            .expect("should authenticate");
        assert_eq!(auth.scope, Scope::Read);
        assert_eq!(auth.actor, "test");
    }

    #[test]
    fn gate_rejects_a_missing_token() {
        let guard = guard_with(&[("bzd_agt_ok", Scope::Read)], 100);
        let err = guard.evaluate(&facts("GET", None)).unwrap_err();
        assert_eq!(err.code, "UNAUTHORIZED");
        assert_eq!(err.status(), 401);
    }

    #[test]
    fn gate_rejects_a_wrong_token() {
        let guard = guard_with(&[("bzd_agt_ok", Scope::Read)], 100);
        let err = guard.evaluate(&facts("GET", Some("Bearer bzd_agt_wrong"))).unwrap_err();
        assert_eq!(err.code, "UNAUTHORIZED");
        assert_eq!(err.status(), 401);
    }

    #[test]
    fn gate_rejects_any_origin_header_even_with_a_valid_token() {
        let guard = guard_with(&[("bzd_agt_ok", Scope::Admin)], 100);
        let mut f = facts("POST", Some("Bearer bzd_agt_ok"));
        f.origin = Some("https://web.whatsapp.com");
        let err = guard.evaluate(&f).unwrap_err();
        assert_eq!(err.status(), 403);
        assert!(err.message.contains("Origin"));
    }

    #[test]
    fn an_origin_header_is_rejected_before_the_token_is_even_looked_at() {
        let guard = guard_with(&[], 100);
        let mut f = facts("POST", None);
        f.origin = Some("https://evil.example");
        let err = guard.evaluate(&f).unwrap_err();
        // If the token were checked first this would still be a 401 UNAUTHORIZED
        // message about a missing bearer token.
        assert!(err.message.contains("Origin"), "got: {}", err.message);
    }

    #[test]
    fn gate_rejects_a_rebound_host() {
        let guard = guard_with(&[("bzd_agt_ok", Scope::Read)], 100);
        let mut f = facts("GET", Some("Bearer bzd_agt_ok"));
        f.host = Some("attacker.example:47821");
        let err = guard.evaluate(&f).unwrap_err();
        assert_eq!(err.status(), 403);
        assert!(err.message.contains("Host"));
    }

    #[test]
    fn gate_rejects_a_missing_host() {
        let guard = guard_with(&[("bzd_agt_ok", Scope::Read)], 100);
        let mut f = facts("GET", Some("Bearer bzd_agt_ok"));
        f.host = None;
        assert_eq!(guard.evaluate(&f).unwrap_err().status(), 403);
    }

    #[test]
    fn gate_never_answers_cors_preflight() {
        let guard = guard_with(&[("bzd_agt_ok", Scope::Admin)], 100);
        let mut f = facts("OPTIONS", Some("Bearer bzd_agt_ok"));
        f.origin = Some("https://evil.example");
        let err = guard.evaluate(&f).unwrap_err();
        assert_eq!(err.code, "METHOD_NOT_ALLOWED");
        assert_eq!(err.status(), 405);
        // Even with no Origin at all, preflight is still refused.
        assert_eq!(
            guard.evaluate(&facts("OPTIONS", Some("Bearer bzd_agt_ok"))).unwrap_err().status(),
            405
        );
    }

    #[test]
    fn gate_rejects_a_non_loopback_peer() {
        let guard = guard_with(&[("bzd_agt_ok", Scope::Admin)], 100);
        let mut f = facts("GET", Some("Bearer bzd_agt_ok"));
        f.peer_is_loopback = false;
        assert_eq!(guard.evaluate(&f).unwrap_err().status(), 403);
    }

    #[test]
    fn gate_enforces_the_rate_limit_after_authentication() {
        let guard = guard_with(&[("bzd_agt_ok", Scope::Read)], 2);
        assert!(guard.evaluate(&facts("GET", Some("Bearer bzd_agt_ok"))).is_ok());
        assert!(guard.evaluate(&facts("GET", Some("Bearer bzd_agt_ok"))).is_ok());
        let err = guard.evaluate(&facts("GET", Some("Bearer bzd_agt_ok"))).unwrap_err();
        assert_eq!(err.code, "RATE_LIMITED");
        assert_eq!(err.status(), 429);
        assert_eq!(err.details.unwrap()["retryAfterSecs"].as_u64().is_some(), true);
    }

    #[test]
    fn a_bad_token_does_not_consume_rate_limit_budget() {
        let guard = guard_with(&[("bzd_agt_ok", Scope::Read)], 1);
        // Failed attempts must not lock the real token out of its own quota.
        for _ in 0..10 {
            assert!(guard.evaluate(&facts("GET", Some("Bearer nope"))).is_err());
        }
        assert!(guard.evaluate(&facts("GET", Some("Bearer bzd_agt_ok"))).is_ok());
    }

    // ---------------- Scope escalation ----------------

    #[test]
    fn scope_error_names_both_scopes() {
        let err = scope_error(Scope::Read, Scope::Admin, "remove_service");
        assert_eq!(err.code, "FORBIDDEN_SCOPE");
        assert_eq!(err.status(), 403);
        let details = err.details.unwrap();
        assert_eq!(details["grantedScope"], "read");
        assert_eq!(details["requiredScope"], "admin");
        assert_eq!(details["target"], "remove_service");
    }

    // ---------------- Routing ----------------

    #[test]
    fn routes_resolve_to_the_expected_handlers() {
        assert_eq!(resolve_route("/api/v1/agent/health"), Some(Route::Health));
        assert_eq!(resolve_route("/api/v1/agent/manifest"), Some(Route::Manifest));
        assert_eq!(resolve_route("/api/v1/agent/openapi.json"), Some(Route::OpenApi));
        assert_eq!(resolve_route("/api/v1/agent/tools"), Some(Route::ListTools));
        assert_eq!(resolve_route("/api/v1/agent/resources"), Some(Route::ListResources));
        assert_eq!(resolve_route("/api/v1/agent/prompts"), Some(Route::ListPrompts));
        assert_eq!(resolve_route("/api/v1/agent/mcp"), Some(Route::Mcp));
        assert_eq!(
            resolve_route("/api/v1/agent/tools/add_service"),
            Some(Route::CallTool("add_service".into()))
        );
        assert_eq!(
            resolve_route("/api/v1/agent/resources/services/abc"),
            Some(Route::ReadResource("services/abc".into()))
        );
        assert_eq!(
            resolve_route("/api/v1/agent/prompts/add-messenger"),
            Some(Route::GetPrompt("add-messenger".into()))
        );
    }

    #[test]
    fn routes_are_tolerant_of_a_trailing_slash() {
        assert_eq!(resolve_route("/api/v1/agent/health/"), Some(Route::Health));
        assert_eq!(resolve_route("/api/v1/agent/tools/"), Some(Route::ListTools));
    }

    #[test]
    fn unknown_or_foreign_paths_do_not_resolve() {
        for path in [
            "/",
            "/api/v1/agent",
            "/api/v1/agent/nope",
            "/api/v2/agent/health",
            "/health",
            "/api/v1/agent/tools/a/b",
            "/api/v1/agent/prompts/a/b",
        ] {
            assert_eq!(resolve_route(path), None, "{path} must not resolve");
        }
    }

    /// A trailing slash is tolerated everywhere, including on whole
    /// collections. This is asserted separately because /resources/ is the
    /// ambiguous-looking case: it could be read as an empty sub-path, and the
    /// collection route must win.
    #[test]
    fn a_trailing_slash_resolves_to_the_collection_not_a_subpath() {
        assert_eq!(resolve_route("/api/v1/agent/resources/"), Some(Route::ListResources));
        assert_eq!(resolve_route("/api/v1/agent/prompts/"), Some(Route::ListPrompts));
        assert_eq!(resolve_route("/api/v1/agent/manifest/"), Some(Route::Manifest));
        // An empty sub-path with nothing after it is still not a resource read.
        assert_eq!(resolve_route("/api/v1/agent/resources//"), Some(Route::ListResources));
    }

    #[test]
    fn route_methods_are_enforced() {
        assert_eq!(Route::CallTool("x".into()).method(), Method::POST);
        assert_eq!(Route::Mcp.method(), Method::POST);
        assert_eq!(Route::Health.method(), Method::GET);
        assert_eq!(Route::ListTools.method(), Method::GET);
    }

    #[test]
    fn resource_fragment_maps_back_to_a_bzdium_uri() {
        assert_eq!(resource_uri("services/abc"), "bzdium://services/abc");
        assert_eq!(resource_uri("app/state"), "bzdium://app/state");
        assert_eq!(resource_uri("/services/abc"), "bzdium://services/abc");
    }

    // ---------------- Decoding ----------------

    #[test]
    fn percent_decoding_handles_escapes_and_rejects_garbage() {
        assert_eq!(percent_decode("add_service").unwrap(), "add_service");
        assert_eq!(percent_decode("a%20b").unwrap(), "a b");
        assert_eq!(percent_decode("bad%"), None);
        assert_eq!(percent_decode("bad%2"), None);
        assert_eq!(percent_decode("bad%zz"), None);
        // A NUL must never survive decoding: it could truncate a path.
        assert_eq!(percent_decode("a%00b"), None);
    }

    #[test]
    fn query_parsing_returns_decoded_pairs() {
        let q = parse_query(Some("a=1&b=hello%20world&c&dryRun=true"));
        assert_eq!(q.get("a").unwrap(), "1");
        assert_eq!(q.get("b").unwrap(), "hello world");
        assert_eq!(q.get("c").unwrap(), "");
        assert_eq!(q.get("dryRun").unwrap(), "true");
        assert!(parse_query(None).is_empty());
    }

    // ---------------- Responses ----------------

    #[test]
    fn responses_never_carry_cors_headers() {
        let resp = error_response(&unauthorized("m", "s"));
        for name in [
            "access-control-allow-origin",
            "access-control-allow-headers",
            "access-control-allow-credentials",
            "access-control-allow-methods",
        ] {
            assert!(resp.headers().get(name).is_none(), "{name} must never be emitted");
        }
        assert_eq!(resp.headers().get("x-content-type-options").unwrap(), "nosniff");
        assert_eq!(resp.headers().get("cache-control").unwrap(), "no-store");
    }

    #[test]
    fn unauthorized_responses_challenge_with_www_authenticate() {
        let resp = error_response(&unauthorized("m", "s"));
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(resp.headers().get("www-authenticate").is_some());
    }

    #[test]
    fn rate_limited_responses_carry_retry_after() {
        let err = AgentError::new(ErrorCode::RateLimited, "m", "s")
            .with_details(json!({ "retryAfterSecs": 17 }));
        let resp = error_response(&err);
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(resp.headers().get("retry-after").unwrap(), "17");
    }

    #[test]
    fn method_not_allowed_responses_advertise_the_allowed_method() {
        let resp = method_response(&Route::CallTool("x".into()));
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(resp.headers().get("allow").unwrap(), "POST");
    }

    #[test]
    fn error_bodies_use_the_agent_envelope() {
        let err = AgentError::new(ErrorCode::UnknownTool, "msg", "do this instead");
        let resp = error_response(&err);
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let value = serde_json::to_value(&err).unwrap();
        assert_eq!(value["code"], "UNKNOWN_TOOL");
        assert_eq!(value["suggestion"], "do this instead");
    }

    // ---------------- Port file ----------------

    #[test]
    fn port_file_round_trips() {
        let dir = std::env::temp_dir().join(format!("bzdium-port-{}", uuid::Uuid::new_v4()));
        assert_eq!(read_port(&dir), None, "absent port file means not running");
        write_port(&dir, 47821).unwrap();
        assert_eq!(read_port(&dir), Some(47821));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_port_file_reads_as_not_running() {
        let dir = std::env::temp_dir().join(format!("bzdium-port2-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(agent_dir(&dir)).unwrap();
        std::fs::write(port_path(&dir), "not a port").unwrap();
        assert_eq!(read_port(&dir), None);
        std::fs::write(port_path(&dir), "0").unwrap();
        assert_eq!(read_port(&dir), None, "port zero is not a usable port");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------- Tokens ----------------

    #[test]
    fn minted_tokens_are_prefixed_unique_and_high_entropy() {
        let a = mint_token(Scope::Read, "a");
        let b = mint_token(Scope::Read, "a");
        assert!(a.token.starts_with("bzd_agt_"));
        assert_ne!(a.token, b.token, "two mints must not collide");
        assert!(a.token.len() >= 8 + 64, "token too short: {}", a.token.len());
        assert!(a.token[8..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a.scope, Scope::Read);
    }

    #[test]
    fn token_store_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("bzdium-tok-{}", uuid::Uuid::new_v4()));
        let mut store = TokenStore::default();
        store.tokens.push(mint_token(Scope::Write, "cli"));
        save_tokens(&dir, &store).unwrap();
        let back = load_tokens(&dir);
        assert_eq!(back.tokens.len(), 1);
        assert_eq!(back.tokens[0].scope, Scope::Write);
        assert!(back.authenticate(&store.tokens[0].token).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_token_file_authenticates_nobody() {
        let dir = std::env::temp_dir().join(format!("bzdium-tok2-{}", uuid::Uuid::new_v4()));
        let store = load_tokens(&dir);
        assert!(store.tokens.is_empty());
        assert!(store.authenticate("bzd_agt_anything").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_token_file_fails_closed() {
        let dir = std::env::temp_dir().join(format!("bzdium-tok3-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(agent_dir(&dir)).unwrap();
        std::fs::write(tokens_path(&dir), b"{ not json").unwrap();
        let store = load_tokens(&dir);
        assert!(store.tokens.is_empty(), "corrupt store must not grant access");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loopback_peers_are_recognised() {
        assert!(is_loopback_peer(&"127.0.0.1:5000".parse().unwrap()));
        assert!(is_loopback_peer(&"[::1]:5000".parse().unwrap()));
        assert!(!is_loopback_peer(&"192.168.1.4:5000".parse().unwrap()));
        assert!(!is_loopback_peer(&"8.8.8.8:5000".parse().unwrap()));
    }
}
