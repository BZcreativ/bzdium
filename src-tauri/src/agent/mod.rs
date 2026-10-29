//! Agent Access Layer: a dual-interface (MCP + REST/OpenAPI 3.1) surface over
//! bzdium's existing command layer.
//!
//! Design invariants
//! -----------------
//! 1. ZERO DUPLICATION. Every tool delegates to the exact same Rust function
//!    the UI invokes over Tauri IPC. There is no second validation path and
//!    no re-implementation of domain rules.
//! 2. ONE SOURCE OF TRUTH FOR SCHEMAS. Each tool declares its JSON Schema
//!    once, in tools.rs; the MCP tools/list, the REST routes and the OpenAPI
//!    3.1 document are all projections of that same table.
//! 3. OFF BY DEFAULT. The loopback listener only starts when the user enables
//!    the agent layer. See load_config.
//! 4. SESSIONS ARE NEVER EXPOSED. Resources expose session directory names
//!    and sizes, never profile contents (cookies, tokens, local storage).

pub mod audit;
pub mod error;
pub mod guard;
pub mod http;
pub mod mcp;
pub mod openapi;
pub mod prompts;
pub mod resources;
pub mod schema;
pub mod scope;
pub mod startup;
pub mod stdio;
pub mod tools;
pub mod wire;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use error::{AgentError, ErrorCode};
use scope::Scope;

/// Everything a tool handler needs. Cheap to clone (AppHandle is refcounted).
#[derive(Clone)]
pub struct AgentCtx {
    pub app: tauri::AppHandle,
    pub data_dir: PathBuf,
    pub started_at: Instant,
}

impl AgentCtx {
    pub fn new(app: tauri::AppHandle) -> Self {
        let data_dir = {
            use tauri::Manager as _;
            let state = app.state::<crate::state::SharedState>();
            let inner = state.lock().expect("state mutex poisoned");
            inner.data_dir.clone()
        };
        Self { app, data_dir, started_at: Instant::now() }
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    pub fn agent_dir(&self) -> PathBuf {
        agent_dir(&self.data_dir)
    }
}

/// The agent directory inside the data dir holds config, tokens, audit log.
pub fn agent_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("agent")
}

// ---------------------------------------------------------------------------
// Tool registry types
// ---------------------------------------------------------------------------

/// A boxed future, so async handlers can be stored in a static table of plain
/// function pointers (no dyn dispatch, no allocation per lookup).
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

pub type ToolHandler = for<'a> fn(
    &'a AgentCtx,
    serde_json::Value,
) -> BoxFuture<'a, Result<serde_json::Value, AgentError>>;

/// One callable capability. This single record feeds MCP tools/list, the REST
/// routes and the OpenAPI document.
pub struct ToolSpec {
    /// Snake_case tool name, identical in MCP and REST.
    pub name: &'static str,
    /// What it does, written for an LLM caller.
    pub description: &'static str,
    /// Minimum scope required.
    pub scope: Scope,
    /// True when the tool can change persisted or runtime state. Mutating
    /// tools accept an optional dryRun parameter.
    pub mutating: bool,
    /// JSON Schema (draft 2020-12 subset) for the input object.
    pub input_schema: fn() -> serde_json::Value,
    pub handler: ToolHandler,
    /// Optional preview handler for dryRun. Runs the same validators and
    /// returns the projected effect without touching state. Mutating tools
    /// should provide one; read-only tools never need it.
    pub preview: Option<ToolHandler>,
}

/// A read-only context document exposed as an MCP resource and a REST GET.
pub struct ResourceSpec {
    /// URI template, e.g. bzdium://services/{id}. Braced segments are params.
    pub uri_template: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub mime_type: &'static str,
    pub scope: Scope,
    /// Renders the resource. Params holds any braced segments from the URI.
    pub render: for<'a> fn(
        &'a AgentCtx,
        &'a std::collections::HashMap<String, String>,
    ) -> BoxFuture<'a, Result<String, AgentError>>,
}

/// A multi-step workflow template surfaced as an MCP prompt.
pub struct PromptSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub scope: Scope,
    pub arguments: fn() -> Vec<PromptArgument>,
    pub render: fn(&std::collections::HashMap<String, String>) -> String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PromptArgument {
    pub name: &'static str,
    pub description: &'static str,
    pub required: bool,
}

// ---------------------------------------------------------------------------
// Runtime configuration (off by default)
// ---------------------------------------------------------------------------

/// agent/config.json inside the data dir. An absent file means disabled.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfig {
    /// Master switch. False unless a human turns it on.
    #[serde(default)]
    pub enabled: bool,
    /// Loopback port. 0 asks the OS for an ephemeral port.
    #[serde(default)]
    pub port: u16,
    /// Permit tokens with admin scope to exist.
    #[serde(default)]
    pub allow_admin_scope: bool,
    /// Requests per minute per token.
    #[serde(default = "default_rate_limit")]
    pub rate_limit_per_minute: u32,
    /// Set on first successful start; read by clients to discover the port.
    #[serde(default)]
    pub last_port: Option<u16>,
    #[serde(default)]
    pub last_started_unix: Option<u64>,
}

fn default_rate_limit() -> u32 {
    120
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 0,
            allow_admin_scope: false,
            rate_limit_per_minute: default_rate_limit(),
            last_port: None,
            last_started_unix: None,
        }
    }
}

pub const CONFIG_FILE: &str = "config.json";
pub const TOKENS_FILE: &str = "tokens.json";
pub const PORT_FILE: &str = "port";
pub const AUDIT_FILE: &str = "audit.jsonl";

/// Creates the agent dir (if needed) restricted to the current user. On
/// multi-user Linux machines the default umask would leave `agent/`
/// world-readable, and tokens.json holds usable bearer keys.
pub fn ensure_agent_dir(data_dir: &Path) -> Result<std::path::PathBuf, String> {
    let dir = agent_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}

/// Tightens a freshly written file to owner-only. Windows has no umask to
/// tighten from (access is governed by the profile-directory ACLs), so this
/// is a no-op there.
#[cfg(unix)]
pub fn restrict_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
pub fn restrict_file(_path: &Path) {}

/// Loads the agent config. A missing or corrupt file resolves to disabled:
/// the layer must fail closed.
pub fn load_config(data_dir: &Path) -> AgentConfig {
    let path = agent_dir(data_dir).join(CONFIG_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            eprintln!("agent: ignoring corrupt {}: {e}", path.display());
            AgentConfig::default()
        }),
        Err(_) => AgentConfig::default(),
    }
}

pub fn save_config(data_dir: &Path, config: &AgentConfig) -> Result<(), String> {
    let dir = ensure_agent_dir(data_dir)?;
    let json = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    let path = dir.join(CONFIG_FILE);
    crate::storage::atomic_write(&path, &json)
        .map_err(|e| format!("cannot write agent config: {e}"))?;
    restrict_file(&path);
    Ok(())
}

/// Shared, immutable snapshot handed to every transport.
pub struct SharedAgent {
    pub ctx: Arc<AgentCtx>,
}

/// Rejects a tool name that is not registered, with a discovery suggestion.
pub fn unknown_tool(name: &str) -> AgentError {
    AgentError::new(
        ErrorCode::UnknownTool,
        format!("No tool named '{name}' is registered."),
        "Call tools/list (MCP) or GET /api/v1/agent/tools (REST) to list valid tool names.",
    )
    .with_details(serde_json::json!({ "tool": name }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_config_defaults_to_disabled() {
        let c = AgentConfig::default();
        assert!(!c.enabled, "the agent layer must be off by default");
        assert!(!c.allow_admin_scope, "admin scope must be off by default");
        assert_eq!(c.rate_limit_per_minute, 120);
    }

    #[test]
    fn corrupt_config_fails_closed() {
        let dir = std::env::temp_dir().join(format!("bzdium-agent-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(agent_dir(&dir)).unwrap();
        std::fs::write(agent_dir(&dir).join(CONFIG_FILE), b"{ not json").unwrap();
        let cfg = load_config(&dir);
        assert!(!cfg.enabled, "corrupt config must resolve to disabled");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_config_file_means_disabled() {
        let dir = std::env::temp_dir().join(format!("bzdium-agent-miss-{}", uuid::Uuid::new_v4()));
        let cfg = load_config(&dir);
        assert!(!cfg.enabled);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_round_trips() {
        let dir = std::env::temp_dir().join(format!("bzdium-agent-rt-{}", uuid::Uuid::new_v4()));
        let mut c = AgentConfig::default();
        c.enabled = true;
        c.port = 47821;
        c.allow_admin_scope = true;
        save_config(&dir, &c).unwrap();
        let back = load_config(&dir);
        assert!(back.enabled);
        assert_eq!(back.port, 47821);
        assert!(back.allow_admin_scope);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
