//! Wiring the desktop settings to the agent layer.
//!
//! This module exists because of one design fact worth stating plainly: the
//! agent layer reads its own config file (agent/config.json) and its own token
//! file (agent/tokens.json), NOT settings.json. That separation is deliberate -
//! the agent's switches carry their own lifecycle and must survive a settings
//! migration - but it means something has to translate a human ticking a
//! checkbox into a written config plus a usable credential.
//!
//! Without this module the layer is unreachable in practice: the checkbox
//! writes settings.json, startup reads config.json, and no code ever mints a
//! first token, so every request would be a 401 with no way to fix it.

use serde::Serialize;

use super::http::{load_tokens, mint_token, save_tokens, tokens_path};
use super::scope::Scope;
use super::{load_config, save_config};

/// What the UI needs to render the Agent Access panel.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    /// Whether the master switch is on. This is the SOURCE OF TRUTH the
    /// listener obeys, not the checkbox position in settings.json.
    pub enabled: bool,
    /// Whether a listener is actually bound in this process right now.
    pub running: bool,
    /// The loopback port, when a listener is up.
    pub port: Option<u16>,
    /// Whether admin-scope tokens may exist (unlocks remove/export/import).
    pub allow_admin_scope: bool,
    /// How many tokens exist. Never the token values themselves.
    pub token_count: usize,
    /// Base URL a client should target, when a listener is up.
    pub base_url: Option<String>,
}

fn read_port(data_dir: &std::path::Path) -> Option<u16> {
    super::http::read_port(data_dir)
}

/// Reads the layer's current state for the settings panel.
pub fn status(data_dir: &std::path::Path) -> AgentStatus {
    let cfg = load_config(data_dir);
    let tokens = load_tokens(data_dir);
    let running = super::startup::is_running();
    let port = if running { read_port(data_dir) } else { None };
    AgentStatus {
        enabled: cfg.enabled,
        running,
        port,
        allow_admin_scope: cfg.allow_admin_scope,
        token_count: tokens.tokens.len(),
        base_url: port.map(|p| format!("http://127.0.0.1:{p}/api/v1/agent")),
    }
}

/// Applies the settings-level switches to the agent config.
///
/// Returns whether the master switch changed, so the caller can start or stop
/// the listener. This is the bridge that was missing: it translates
/// settings.agent_access into agent/config.json.enabled.
pub fn sync_from_settings(
    data_dir: &std::path::Path,
    agent_access: bool,
    allow_admin_scope: bool,
) -> Result<bool, String> {
    let mut cfg = load_config(data_dir);
    let changed = cfg.enabled != agent_access;
    cfg.enabled = agent_access;
    // Admin scope is granted only by an explicit request, never inherited.
    cfg.allow_admin_scope = allow_admin_scope;
    save_config(data_dir, &cfg)?;
    Ok(changed)
}

/// Ensures at least one usable token exists, minting a write-scope token when
/// the store is empty. Returns the plaintext token ONLY when it was just
/// created, because after this call it is stored and never shown again.
pub fn ensure_first_token(
    data_dir: &std::path::Path,
    allow_admin_scope: bool,
) -> Result<Option<String>, String> {
    let mut store = load_tokens(data_dir);
    if !store.tokens.is_empty() {
        return Ok(None);
    }
    // Write scope covers every non-destructive capability, which is what a
    // first-time user expects: read state and manage services, but not delete
    // a service or replace the whole configuration.
    //
    // Deliberately Write even when admin scope is allowed. Enabling admin is a
    // capability ceiling ("an admin token MAY exist"), not an instruction to
    // grant one: silently handing the bootstrap credential the power to delete
    // every saved login because a ceiling was open is exactly the kind of
    // ambient authority that a scoped-credential design exists to prevent. An
    // admin token has to be asked for explicitly, per token.
    let scope = Scope::Write;
    let _ = allow_admin_scope;
    let token = mint_token(scope, "Created from Settings");
    let plaintext = token.token.clone();
    store.tokens.push(token);
    save_tokens(data_dir, &store)?;
    Ok(Some(plaintext))
}

/// Mints an additional token. Used by the UI so a user can hand a read-only
/// credential to a monitoring tool without sharing their write token.
pub fn mint_additional_token(
    data_dir: &std::path::Path,
    scope: Scope,
    label: &str,
) -> Result<String, String> {
    let cfg = load_config(data_dir);
    if scope == Scope::Admin && !cfg.allow_admin_scope {
        return Err(
            "Admin-scope tokens are disabled. Enable 'Allow admin-scope tools' first, and understand that this permits service deletion and configuration replacement.".to_string(),
        );
    }
    let mut store = load_tokens(data_dir);
    let token = mint_token(scope, label);
    let plaintext = token.token.clone();
    store.tokens.push(token);
    save_tokens(data_dir, &store)?;
    Ok(plaintext)
}

/// Removes every token and deletes the token file. The listener stays up but
/// authenticates nobody, which is the correct fail-closed state: an enabled
/// layer with no credential grants nothing.
pub fn revoke_all_tokens(data_dir: &std::path::Path) -> Result<usize, String> {
    let store = load_tokens(data_dir);
    let removed = store.tokens.len();
    let path = tokens_path(data_dir);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(removed),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(removed),
        Err(e) => Err(format!("cannot remove {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("bzdium-agent-wire-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn sync_turns_the_layer_on_and_reports_the_change() {
        let dir = temp_dir();
        assert!(!load_config(&dir).enabled, "must start disabled");

        let changed = sync_from_settings(&dir, true, false).unwrap();
        assert!(changed, "enabling must be reported as a change");
        assert!(load_config(&dir).enabled, "config must now be enabled");
        assert!(!load_config(&dir).allow_admin_scope);

        // Idempotent: a second identical sync is not a change.
        assert!(!sync_from_settings(&dir, true, false).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_off_disables_and_is_reversible() {
        let dir = temp_dir();
        sync_from_settings(&dir, true, false).unwrap();
        assert!(load_config(&dir).enabled);
        let changed = sync_from_settings(&dir, false, false).unwrap();
        assert!(changed);
        assert!(!load_config(&dir).enabled, "switching off must persist");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn admin_scope_is_only_granted_when_explicitly_requested() {
        let dir = temp_dir();
        sync_from_settings(&dir, true, false).unwrap();
        assert!(!load_config(&dir).allow_admin_scope, "must not grant admin implicitly");

        sync_from_settings(&dir, true, true).unwrap();
        assert!(load_config(&dir).allow_admin_scope, "explicit opt-in must stick");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_token_is_minted_once_and_returned_only_then() {
        let dir = temp_dir();
        sync_from_settings(&dir, true, false).unwrap();

        let first = ensure_first_token(&dir, false).unwrap();
        let plaintext = first.expect("the first call must surface the token");
        assert!(plaintext.starts_with("bzd_agt_"), "token must carry its prefix");

        // It is now persisted, so it must not be handed out again.
        assert!(
            ensure_first_token(&dir, false).unwrap().is_none(),
            "a second call must not mint or reveal another token"
        );
        assert_eq!(load_tokens(&dir).tokens.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_first_token_is_always_write_scoped_even_with_admin_allowed() {
        let dir = temp_dir();
        let t = ensure_first_token(&dir, false).unwrap().unwrap();
        let store = load_tokens(&dir);
        assert_eq!(store.tokens[0].scope, Scope::Write);
        assert_eq!(store.authenticate(&t).map(|t| t.scope), Some(Scope::Write));

        // Allowing admin scope raises the ceiling; it must NOT hand the
        // bootstrap credential admin power. If it did, a user who merely
        // opened the ceiling would silently own a key that can delete saved
        // logins and replace the entire configuration.
        let dir2 = temp_dir();
        ensure_first_token(&dir2, true).unwrap();
        assert_eq!(
            load_tokens(&dir2).tokens[0].scope,
            Scope::Write,
            "the first token must stay write-scoped even when admin is allowed"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn minting_an_admin_token_is_refused_until_admin_is_enabled() {
        let dir = temp_dir();
        sync_from_settings(&dir, true, false).unwrap();
        let err = mint_additional_token(&dir, Scope::Admin, "danger").unwrap_err();
        assert!(err.contains("Admin-scope tokens are disabled"), "got: {err}");

        sync_from_settings(&dir, true, true).unwrap();
        let ok = mint_additional_token(&dir, Scope::Admin, "danger").unwrap();
        assert!(ok.starts_with("bzd_agt_"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn revoking_all_tokens_leaves_the_layer_authenticating_nobody() {
        let dir = temp_dir();
        let t = ensure_first_token(&dir, false).unwrap().unwrap();
        assert_eq!(revoke_all_tokens(&dir).unwrap(), 1);

        let store = load_tokens(&dir);
        assert!(store.tokens.is_empty());
        assert!(
            store.authenticate(&t).is_none(),
            "a revoked token must not authenticate"
        );
        // Revoking twice is not an error.
        assert_eq!(revoke_all_tokens(&dir).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_reports_off_before_anything_is_enabled() {
        let dir = temp_dir();
        let s = status(&dir);
        assert!(!s.enabled);
        assert!(!s.running);
        assert_eq!(s.token_count, 0);
        assert!(s.base_url.is_none(), "no port before the listener runs");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_never_leaks_a_token_value() {
        let dir = temp_dir();
        let plaintext = ensure_first_token(&dir, false).unwrap().unwrap();
        let rendered = serde_json::to_string(&status(&dir)).unwrap();
        assert!(
            !rendered.contains(&plaintext),
            "the status payload must never contain the credential: {rendered}"
        );
        assert!(rendered.contains("\"tokenCount\":1"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
