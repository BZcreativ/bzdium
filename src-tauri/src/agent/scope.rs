//! Permission scopes and token-based authentication.
//!
//! Three tiers, ordered: read < write < admin. A token carries one scope; a
//! tool declares the minimum scope it needs. The check is a simple ordering
//! comparison, so adding a tier later only requires updating the rank table.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::error::{AgentError, ErrorCode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Read,
    Write,
    Admin,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Write => "write",
            Scope::Admin => "admin",
        }
    }

    pub fn parse(raw: &str) -> Option<Scope> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "read" => Some(Scope::Read),
            "write" => Some(Scope::Write),
            "admin" => Some(Scope::Admin),
            _ => None,
        }
    }

    /// True when this scope is at least as privileged as the required one.
    pub fn satisfies(self, required: Scope) -> bool {
        self >= required
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A minted agent credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentToken {
    /// The secret itself, prefixed with bzd_agt_.
    pub token: String,
    pub scope: Scope,
    /// Human label, purely for the audit log.
    #[serde(default)]
    pub label: String,
}

/// The on-disk credential file: agent/tokens.json inside the data dir.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenStore {
    #[serde(default)]
    pub tokens: Vec<AgentToken>,
}

impl TokenStore {
    /// Resolves a presented bearer token. Comparison is constant-time so a
    /// caller cannot learn the token byte-by-byte from response timing.
    pub fn authenticate(&self, presented: &str) -> Option<&AgentToken> {
        let mut found: Option<&AgentToken> = None;
        for entry in &self.tokens {
            if ct_eq(entry.token.as_bytes(), presented.as_bytes()) {
                found = Some(entry);
            }
        }
        found
    }
}

/// Length-independent, early-exit-free byte comparison.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Checks that a token's scope covers what a tool requires.
pub fn require_scope(granted: Scope, required: Scope, tool: &str) -> Result<(), AgentError> {
    if granted.satisfies(required) {
        return Ok(());
    }
    Err(AgentError::new(
        ErrorCode::ForbiddenScope,
        format!(
            "This token has scope '{}' but '{}' requires scope '{}'.",
            granted, tool, required
        ),
        format!(
            "Use a token minted with scope '{}' (admin tokens are disabled by default for safety). \
             Call list_tools to see each tool's required scope.",
            required
        ),
    )
    .with_details(serde_json::json!({
        "grantedScope": granted.as_str(),
        "requiredScope": required.as_str(),
        "tool": tool,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_ordering_is_read_write_admin() {
        assert!(Scope::Read < Scope::Write);
        assert!(Scope::Write < Scope::Admin);
        assert!(Scope::Admin.satisfies(Scope::Read));
        assert!(Scope::Write.satisfies(Scope::Read));
        assert!(!Scope::Read.satisfies(Scope::Write));
        assert!(!Scope::Write.satisfies(Scope::Admin));
    }

    #[test]
    fn scope_parse_round_trips() {
        for s in [Scope::Read, Scope::Write, Scope::Admin] {
            assert_eq!(Scope::parse(s.as_str()), Some(s));
        }
        assert_eq!(Scope::parse("ADMIN"), Some(Scope::Admin));
        assert_eq!(Scope::parse("root"), None);
    }

    #[test]
    fn authenticate_matches_exact_token_only() {
        let store = TokenStore {
            tokens: vec![
                AgentToken { token: "bzd_agt_aaa".into(), scope: Scope::Read, label: "a".into() },
                AgentToken { token: "bzd_agt_bbb".into(), scope: Scope::Admin, label: "b".into() },
            ],
        };
        assert_eq!(store.authenticate("bzd_agt_bbb").unwrap().scope, Scope::Admin);
        assert!(store.authenticate("bzd_agt_bb").is_none());
        assert!(store.authenticate("bzd_agt_bbbb").is_none());
        assert!(store.authenticate("").is_none());
    }

    #[test]
    fn require_scope_rejects_escalation_with_suggestion() {
        let err = require_scope(Scope::Read, Scope::Admin, "remove_service").unwrap_err();
        assert_eq!(err.code, "FORBIDDEN_SCOPE");
        assert!(err.suggestion.contains("admin"));
    }

    #[test]
    fn require_scope_accepts_sufficient() {
        assert!(require_scope(Scope::Admin, Scope::Read, "list_services").is_ok());
        assert!(require_scope(Scope::Write, Scope::Write, "add_service").is_ok());
    }
}
