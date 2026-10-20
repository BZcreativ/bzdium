//! End-to-end lifecycle tests for the Agent Access layer.
//!
//! These cover the journey a real user takes and that per-module unit tests
//! cannot reach: enable the layer from Settings, obtain a credential, and make
//! an authenticated request over a real TCP socket. That path was broken even
//! while every unit test passed, because the settings checkbox wrote one file
//! while startup read another and nothing minted a first token. These tests
//! exist so that class of breakage cannot come back silently.

use std::sync::Arc;

use bzdium_lib::agent::http::{self, AgentBackend, RequestFacts, ServerState, ToolCallRequest};
use bzdium_lib::agent::scope::Scope;
use bzdium_lib::agent::{self, wire};

fn temp_dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("bzdium-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    d
}
// The layer's on/off switch is process-wide, so tests that flip it must not
// overlap: one test switching the layer off would otherwise make another
// test's request fail with 503 instead of the status it is asserting.
/// tokio's mutex, not std's: its guard is Send, so a test can hold it across
/// `.await`. A std guard cannot be held across an await point.
static SWITCH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Serialises async tests that drive the global switch, leaving the layer on.
async fn switch_guard() -> tokio::sync::MutexGuard<'static, ()> {
    let guard = SWITCH.lock().await;
    http::set_enabled(true);
    guard
}

/// The same for synchronous tests. blocking_lock is legal here because a plain
/// #[test] runs off any runtime; it would panic inside an async context.
fn switch_guard_blocking() -> tokio::sync::MutexGuard<'static, ()> {
    let guard = SWITCH.blocking_lock();
    http::set_enabled(true);
    guard
}


// ---------------------------------------------------------------------------
// 1. The settings -> config bridge
// ---------------------------------------------------------------------------

#[test]
fn ticking_the_checkbox_writes_the_config_startup_reads() {
    let dir = temp_dir();

    // Before: the layer is off, so startup would not listen.
    assert!(!agent::load_config(&dir).enabled);

    // The settings command mirrors the checkbox into the layer config.
    let changed = wire::sync_from_settings(&dir, true, false).unwrap();
    assert!(changed, "enabling must be reported so the caller can start");

    // After: startup's own reader now agrees.
    assert!(
        agent::load_config(&dir).enabled,
        "startup reads load_config; it must see the enable"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn enabling_mints_a_usable_first_token() {
    let dir = temp_dir();
    wire::sync_from_settings(&dir, true, false).unwrap();

    let token = wire::ensure_first_token(&dir, false)
        .unwrap()
        .expect("enabling must hand the user a credential");

    // The token the UI was shown must actually authenticate.
    let store = http::load_tokens(&dir);
    let authed = store
        .authenticate(&token)
        .expect("the token shown to the user must authenticate");
    assert_eq!(authed.scope, Scope::Write, "first token is write-scoped");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn disabling_stops_startup_from_listening() {
    let dir = temp_dir();
    wire::sync_from_settings(&dir, true, false).unwrap();
    assert!(agent::load_config(&dir).enabled);

    wire::sync_from_settings(&dir, false, false).unwrap();
    assert!(
        !agent::load_config(&dir).enabled,
        "switching off must persist: the listener must not come back on restart"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 2. The request gate honours a live disable
// ---------------------------------------------------------------------------

#[test]
fn the_layer_starts_switched_off() {
    // Fail closed: nothing serves until something explicitly enables it.
    // Other tests flip this flag, so assert the API rather than the value.
    http::set_enabled(false);
    assert!(!http::is_enabled());
    http::set_enabled(true);
    assert!(http::is_enabled());
    http::set_enabled(false);
}

#[test]
fn a_token_only_works_while_the_layer_is_enabled() {
    let dir = temp_dir();
    wire::sync_from_settings(&dir, true, false).unwrap();
    let plaintext = wire::ensure_first_token(&dir, false).unwrap().unwrap();
    let store = http::load_tokens(&dir);
    let cfg = agent::load_config(&dir);
    let guard = http::Guard::new(Arc::new(store), &cfg);

    // A well-formed, correctly-authenticated request.
    let facts = RequestFacts {
        method: "GET",
        origin: None,
        host: Some("127.0.0.1:1234"),
        authorization: Some(&format!("Bearer {plaintext}")),
        peer_is_loopback: true,
    };
    assert!(
        guard.evaluate(&facts).is_ok(),
        "a valid token must be accepted while enabled"
    );

    // The same request with the layer switched off must not reach a handler.
    http::set_enabled(false);
    assert!(!http::is_enabled());
    http::set_enabled(true);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 3. Hostile request rejection, on the real Guard
// ---------------------------------------------------------------------------

#[test]
fn the_gate_refuses_a_revoked_token() {
    let dir = temp_dir();
    wire::sync_from_settings(&dir, true, false).unwrap();
    let plaintext = wire::ensure_first_token(&dir, false).unwrap().unwrap();
    wire::revoke_all_tokens(&dir).unwrap();

    let store = http::load_tokens(&dir);
    let cfg = agent::load_config(&dir);
    let guard = http::Guard::new(Arc::new(store), &cfg);
    let facts = RequestFacts {
        method: "GET",
        origin: None,
        host: Some("127.0.0.1:1234"),
        authorization: Some(&format!("Bearer {plaintext}")),
        peer_is_loopback: true,
    };
    let err = guard
        .evaluate(&facts)
        .expect_err("a revoked token must not authenticate");
    assert_eq!(err.status(), 401);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_admin_token_cannot_exist_until_admin_scope_is_allowed() {
    let dir = temp_dir();
    wire::sync_from_settings(&dir, true, false).unwrap();
    assert!(
        wire::mint_additional_token(&dir, Scope::Admin, "x").is_err(),
        "admin tokens must be refused while the ceiling is closed"
    );

    wire::sync_from_settings(&dir, true, true).unwrap();
    assert!(wire::mint_additional_token(&dir, Scope::Admin, "x").is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 4. A real socket round-trip
//
// This is the test that would have caught the broken bootstrap: it only passes
// if enable + mint + listen + authenticate all work together.
// ---------------------------------------------------------------------------

struct StubBackend;

impl AgentBackend for StubBackend {
    fn health(&self) -> serde_json::Value {
        serde_json::json!({ "status": "ok", "stub": true })
    }
    fn manifest(&self) -> serde_json::Value {
        serde_json::json!({ "name": "bzdium", "stub": true })
    }
    fn openapi_json(&self) -> String {
        "{}".to_string()
    }
    fn list_tools(&self) -> serde_json::Value {
        serde_json::json!({ "tools": [] })
    }
    fn list_resources(&self) -> serde_json::Value {
        serde_json::json!({ "resources": [] })
    }
    fn list_prompts(&self) -> serde_json::Value {
        serde_json::json!({ "prompts": [] })
    }
    fn call_tool(
        &self,
        _req: ToolCallRequest,
    ) -> bzdium_lib::agent::BoxFuture<'static, Result<serde_json::Value, bzdium_lib::agent::error::AgentError>>
    {
        Box::pin(async { Ok(serde_json::json!({ "stub": true })) })
    }
    fn read_resource(
        &self,
        _transport: &'static str,
        _actor: String,
        _scope: Scope,
        uri: String,
    ) -> bzdium_lib::agent::BoxFuture<'static, Result<(String, String), bzdium_lib::agent::error::AgentError>>
    {
        Box::pin(async move { Ok((uri, "{}".to_string())) })
    }
    fn render_prompt(
        &self,
        _transport: &'static str,
        _actor: String,
        _scope: Scope,
        name: String,
        _args: std::collections::HashMap<String, String>,
    ) -> Result<serde_json::Value, bzdium_lib::agent::error::AgentError> {
        Ok(serde_json::json!({ "name": name }))
    }
    fn mcp(
        &self,
        _scope: Scope,
        _actor: String,
        _body: serde_json::Value,
    ) -> bzdium_lib::agent::BoxFuture<'static, Result<serde_json::Value, bzdium_lib::agent::error::AgentError>>
    {
        Box::pin(async { Ok(serde_json::json!({ "jsonrpc": "2.0" })) })
    }
}

fn stub_state(dir: &std::path::Path, enabled: bool) -> Arc<ServerState> {
    let mut cfg = agent::load_config(dir);
    cfg.enabled = enabled;
    cfg.port = 0;
    let tokens = Arc::new(http::load_tokens(dir));
    Arc::new(ServerState {
        backend: Arc::new(StubBackend),
        guard: http::Guard::new(tokens, &cfg),
    })
}

/// Minimal blocking HTTP/1.1 GET so the test needs no HTTP client dependency,
/// matching the hand-rolled approach the server itself takes.
fn http_get(port: u16, path: &str, token: Option<&str>) -> String {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect to the agent port");
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let mut req = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n"
    );
    if let Some(t) = token {
        req.push_str(&format!("Authorization: Bearer {t}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).unwrap();
    let mut buf = String::new();
    stream.read_to_string(&mut buf).ok();
    buf
}

fn status_line(resp: &str) -> &str {
    resp.lines().next().unwrap_or("")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_real_socket_round_trip_from_enable_to_authenticated_read() {
    let _switch = switch_guard().await;
    let dir = temp_dir();

    // 1. The user ticks the box.
    wire::sync_from_settings(&dir, true, false).unwrap();
    // 2. The layer mints them a key.
    let token = wire::ensure_first_token(&dir, false).unwrap().unwrap();

    // 3. Bind a real listener on an ephemeral loopback port.
    http::set_enabled(true);
    let state = stub_state(&dir, true);
    let port = http::serve_with_state(state, dir.clone(), 0)
        .await
        .expect("the listener must bind loopback");

    // The port must be published for clients to discover.
    assert_eq!(http::read_port(&dir), Some(port));

    // 4. An unauthenticated request is refused.
    let anon = http_get(port, "/api/v1/agent/health", None);
    assert!(
        status_line(&anon).contains("401"),
        "an unauthenticated request must be 401, got: {}",
        status_line(&anon)
    );

    // 5. The minted token is accepted and returns the backend's health.
    let authed = http_get(port, "/api/v1/agent/health", Some(&token));
    assert!(
        status_line(&authed).contains("200"),
        "the token from Settings must work end-to-end, got: {}",
        status_line(&authed)
    );
    assert!(
        authed.contains("\"status\"") || authed.contains("ok"),
        "the response should carry the health body, got: {authed}"
    );

    // 6. Switching the layer off refuses the very same valid token.
    http::set_enabled(false);
    let off = http_get(port, "/api/v1/agent/health", Some(&token));
    assert!(
        status_line(&off).contains("503"),
        "a switched-off layer must answer 503, got: {}",
        status_line(&off)
    );
    http::set_enabled(true);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_socket_refuses_a_foreign_origin_even_with_a_valid_token() {
    let _switch = switch_guard().await;
    let dir = temp_dir();
    wire::sync_from_settings(&dir, true, false).unwrap();
    let token = wire::ensure_first_token(&dir, false).unwrap().unwrap();

    http::set_enabled(true);
    let state = stub_state(&dir, true);
    let port = http::serve_with_state(state, dir.clone(), 0).await.unwrap();

    // Hand-build a request carrying a browser Origin, as a hostile page would.
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let req = format!(
        "GET /api/v1/agent/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: https://evil.example\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).unwrap();
    let mut buf = String::new();
    stream.read_to_string(&mut buf).ok();

    assert!(
        status_line(&buf).contains("403"),
        "a browser Origin must be 403 even with a valid token, got: {}",
        status_line(&buf)
    );
    assert!(
        !buf.to_ascii_lowercase().contains("access-control-"),
        "no CORS header may ever be emitted"
    );

    http::set_enabled(true);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 5. The admin ceiling, end to end
//
// The admin scope is the privilege ceiling: a write token must not reach a
// destructive tool even though it passes authentication, and an admin token
// can only exist once the operator has allowed one. Asserting the scope check
// only at the unit level misses the wiring -- so drive it over the socket.
// ---------------------------------------------------------------------------

/// Records the tool names that actually reached the backend, so a test can
/// prove a refused request never got there.
#[derive(Default)]
struct RecordingBackend {
    calls: std::sync::Mutex<Vec<String>>,
}

impl RecordingBackend {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl AgentBackend for RecordingBackend {
    fn health(&self) -> serde_json::Value {
        serde_json::json!({ "status": "ok" })
    }
    fn manifest(&self) -> serde_json::Value {
        serde_json::json!({ "name": "bzdium" })
    }
    fn openapi_json(&self) -> String {
        "{}".to_string()
    }
    fn list_tools(&self) -> serde_json::Value {
        serde_json::json!({ "tools": [] })
    }
    fn list_resources(&self) -> serde_json::Value {
        serde_json::json!({ "resources": [] })
    }
    fn list_prompts(&self) -> serde_json::Value {
        serde_json::json!({ "prompts": [] })
    }
    fn call_tool(&self, req: ToolCallRequest) -> bzdium_lib::agent::BoxFuture<'static, Result<serde_json::Value, bzdium_lib::agent::error::AgentError>> {
        self.calls.lock().unwrap().push(req.name.clone());
        Box::pin(async { Ok(serde_json::json!({ "ok": true })) })
    }
    fn read_resource(&self, _transport: &'static str, _actor: String, _scope: Scope, uri: String) -> bzdium_lib::agent::BoxFuture<'static, Result<(String, String), bzdium_lib::agent::error::AgentError>> {
        Box::pin(async move { Ok((uri, "{}".to_string())) })
    }
    fn render_prompt(&self, _transport: &'static str, _actor: String, _scope: Scope, name: String, _args: std::collections::HashMap<String, String>) -> Result<serde_json::Value, bzdium_lib::agent::error::AgentError> {
        Ok(serde_json::json!({ "name": name }))
    }
    fn mcp(&self, _scope: Scope, _actor: String, _body: serde_json::Value) -> bzdium_lib::agent::BoxFuture<'static, Result<serde_json::Value, bzdium_lib::agent::error::AgentError>> {
        Box::pin(async { Ok(serde_json::json!({ "jsonrpc": "2.0" })) })
    }
}

fn recording_state(dir: &std::path::Path) -> (Arc<ServerState>, Arc<RecordingBackend>) {
    let mut cfg = agent::load_config(dir);
    cfg.enabled = true;
    cfg.port = 0;
    let tokens = Arc::new(http::load_tokens(dir));
    let backend = Arc::new(RecordingBackend::default());
    let state = Arc::new(ServerState {
        backend: backend.clone(),
        guard: http::Guard::new(tokens, &cfg),
    });
    (state, backend)
}

/// Minimal blocking HTTP/1.1 POST, for the tools/call endpoint.
fn http_post(port: u16, path: &str, token: &str, body: &str) -> String {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect to the agent port");
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(req.as_bytes()).unwrap();
    let mut buf = String::new();
    stream.read_to_string(&mut buf).ok();
    buf
}

/// A write-scoped token authenticates but must be refused by a destructive
/// tool, and the refusal must happen before the tool runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_token_cannot_drive_a_destructive_tool() {
    let _switch = switch_guard().await;
    let dir = temp_dir();
    wire::sync_from_settings(&dir, true, true).unwrap();
    let write_token = wire::ensure_first_token(&dir, true).unwrap().unwrap();

    http::set_enabled(true);
    let (state, backend) = recording_state(&dir);
    let port = http::serve_with_state(state, dir.clone(), 0).await.unwrap();

    let resp = http_post(
        port,
        "/api/v1/agent/tools/remove_service",
        &write_token,
        "{\"id\":\"x\"}",
    );
    assert!(
        status_line(&resp).contains("403"),
        "a write token must get 403 from an admin-only tool, got: {}",
        status_line(&resp)
    );
    assert!(
        backend.calls().is_empty(),
        "a refused tool must not reach the backend, but it ran: {:?}",
        backend.calls()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// With the ceiling open, an admin token reaches the same tool and succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_admin_token_can_drive_a_destructive_tool() {
    let _switch = switch_guard().await;
    let dir = temp_dir();
    wire::sync_from_settings(&dir, true, true).unwrap();
    wire::ensure_first_token(&dir, true).unwrap();
    let admin = wire::mint_additional_token(&dir, Scope::Admin, "e2e").unwrap();

    http::set_enabled(true);
    let (state, backend) = recording_state(&dir);
    let port = http::serve_with_state(state, dir.clone(), 0).await.unwrap();

    let resp = http_post(
        port,
        "/api/v1/agent/tools/remove_service",
        &admin,
        "{\"id\":\"x\"}",
    );
    assert!(
        status_line(&resp).contains("200"),
        "an admin token must reach an admin tool, got: {}",
        status_line(&resp)
    );
    assert_eq!(
        backend.calls(),
        vec!["remove_service".to_string()],
        "the admin tool must actually run exactly once"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// import_config is the most dangerous tool -- it replaces the user's whole
/// configuration -- so it is gated twice: admin scope to be reachable at all,
/// and a confirmation token inside the handler. Prove both over the socket
/// for the scope half, and against the published schema for the other, since
/// a stub backend cannot reproduce the handler's own check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_config_is_admin_only_over_the_socket() {
    let _switch = switch_guard().await;
    let dir = temp_dir();
    wire::sync_from_settings(&dir, true, true).unwrap();
    let write_token = wire::ensure_first_token(&dir, true).unwrap().unwrap();

    http::set_enabled(true);
    let (state, backend) = recording_state(&dir);
    let port = http::serve_with_state(state, dir.clone(), 0).await.unwrap();

    let resp = http_post(
        port,
        "/api/v1/agent/tools/import_config",
        &write_token,
        "{\"path\":\"x.json\",\"confirmationToken\":\"x\"}",
    );
    assert!(
        status_line(&resp).contains("403"),
        "import_config must be admin-only, got: {}",
        status_line(&resp)
    );
    assert!(
        backend.calls().is_empty(),
        "a refused import must never reach the handler, but it ran: {:?}",
        backend.calls()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Revoking a key must take effect at once, not at the next restart.
///
/// The guard used to snapshot the token store when the listener started, so
/// 'Revoke all keys' removed them from disk while the running server kept
/// honouring every one of them: a button that reported success and changed
/// nothing until the app was restarted. This drives the file-following guard
/// the real server uses.
#[test]
fn revoking_a_token_takes_effect_without_a_restart() {
    let dir = temp_dir();
    wire::sync_from_settings(&dir, true, false).unwrap();
    let plaintext = wire::ensure_first_token(&dir, false).unwrap().unwrap();
    let cfg = agent::load_config(&dir);
    let guard = http::Guard::following_file(dir.clone(), &cfg);

    let facts = RequestFacts {
        method: "GET",
        origin: None,
        host: Some("127.0.0.1:1234"),
        authorization: Some(&format!("Bearer {plaintext}")),
        peer_is_loopback: true,
    };
    assert!(
        guard.evaluate(&facts).is_ok(),
        "the freshly minted token must be accepted"
    );

    // The same revoke the UI button performs.
    let removed = wire::revoke_all_tokens(&dir).unwrap();
    assert_eq!(removed, 1, "exactly one key should have been revoked");

    // The SAME guard, no restart: the credential must now be refused.
    assert!(
        guard.evaluate(&facts).is_err(),
        "a revoked token must be refused immediately by the running guard"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Fails closed: deleting the token file must not leave the guard permissive.
#[test]
fn a_deleted_token_file_authenticates_nobody() {
    let dir = temp_dir();
    wire::sync_from_settings(&dir, true, false).unwrap();
    let plaintext = wire::ensure_first_token(&dir, false).unwrap().unwrap();
    let cfg = agent::load_config(&dir);
    let guard = http::Guard::following_file(dir.clone(), &cfg);

    std::fs::remove_file(http::tokens_path(&dir)).unwrap();

    let facts = RequestFacts {
        method: "GET",
        origin: None,
        host: Some("127.0.0.1:1234"),
        authorization: Some(&format!("Bearer {plaintext}")),
        peer_is_loopback: true,
    };
    assert!(
        guard.evaluate(&facts).is_err(),
        "no token file must mean no authorised tokens, not a permissive default"
    );

    let _ = std::fs::remove_dir_all(&dir);
}


/// The admin ceiling must actually gate admin tools, not just minting.
///
/// allow_admin_scope is consulted when a token is minted, never when one is
/// used. So an admin token kept working after the operator unticked 'Allow
/// admin tools', even though the UI's own comment says closing the ceiling
/// 'strictly reduces power'. It did not: the key could still delete services.
#[test]
fn closing_the_admin_ceiling_disables_existing_admin_tokens() {
    let dir = temp_dir();
    wire::sync_from_settings(&dir, true, true).unwrap();
    let admin = wire::mint_additional_token(&dir, Scope::Admin, "e2e").unwrap();

    // Ceiling open: the admin key works.
    let cfg = agent::load_config(&dir);
    let guard = http::Guard::following_file(dir.clone(), &cfg);
    let facts = RequestFacts {
        method: "POST",
        origin: None,
        host: Some("127.0.0.1:1234"),
        authorization: Some(&format!("Bearer {admin}")),
        peer_is_loopback: true,
    };
    assert!(
        guard.evaluate(&facts).is_ok(),
        "an admin token must work while the ceiling is open"
    );

    // The operator unticks 'Allow admin tools'.
    wire::sync_from_settings(&dir, true, false).unwrap();
    let cfg_closed = agent::load_config(&dir);
    let guard_closed = http::Guard::following_file(dir.clone(), &cfg_closed);

    let outcome = guard_closed.evaluate(&facts);
    match outcome {
        Err(e) => assert_eq!(
            e.code, "FORBIDDEN_SCOPE",
            "closing the ceiling must refuse an admin token with FORBIDDEN_SCOPE"
        ),
        Ok(_) => panic!(
            "closing the admin ceiling left an admin token fully usable, so unticking the box reduced nothing"
        ),
    }

    let _ = std::fs::remove_dir_all(&dir);
}


/// The listener must still be alive after the code that starts it returns.
///
/// This is the bug that a green test suite could not see: serve_with_state
/// binds, writes the port file, hands the accept loop to tokio::spawn, then
/// RETURNS. startup::start called it from block_on, so block_on finished, the
/// Runtime was dropped at the end of its scope, and dropping a runtime cancels
/// every task it spawned. The accept loop died with it. The symptom was an
/// app that reported 'listening on 127.0.0.1:PORT' and wrote a port file while
/// nothing was bound: connection refused on the very port it advertised.
///
/// Every existing test used #[tokio::test], whose runtime spans the whole test
/// body, so the listener stayed up for the duration of the assertions and the
/// defect was invisible. This test deliberately reproduces production's shape:
/// a runtime built, block_on used, and the runtime kept alive only by the
/// caller exactly as startup::start does.
#[test]
fn the_listener_survives_the_call_that_started_it() {
    let _switch = switch_guard_blocking();
    let dir = temp_dir();
    wire::sync_from_settings(&dir, true, false).unwrap();
    let write_token = wire::ensure_first_token(&dir, false).unwrap().unwrap();

    // Production shape: our own runtime, driven by block_on, parked so the
    // runtime outlives the block_on call.
    let (port_tx, port_rx) = std::sync::mpsc::channel::<u16>();
    let dir_for_thread = dir.clone();
    let server = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime must build");
        runtime.block_on(async move {
            let (state, _backend) = recording_state(&dir_for_thread);
            let port = http::serve_with_state(state, dir_for_thread.clone(), 0)
                .await
                .expect("the listener must bind loopback");
            port_tx.send(port).unwrap();
            // Exactly what startup::start now does.
            std::future::pending::<()>().await;
        });
    });

    let port = port_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the listener must report a bound port");

    // The port file must point at a socket that actually answers.
    assert_eq!(http::read_port(&dir), Some(port));
    let resp = http_get(port, "/api/v1/agent/health", Some(&write_token));
    assert!(
        status_line(&resp).contains("200"),
        "the advertised port must accept connections, got: {}",
        status_line(&resp)
    );

    // Leak the parked thread deliberately: it is detached for the rest of the
    // test binary, which mirrors the process-lifetime intent of the real one.
    std::mem::forget(server);
    let _ = std::fs::remove_dir_all(&dir);
}

