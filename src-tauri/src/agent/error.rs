//! Agent-facing error contract.
//!
//! Every failure an agent can observe is expressed as AgentError, which
//! serialises to the PRD's required shape:
//!
//!   { "error": { "code": "...", "message": "...", "suggestion": "...", "details": {...} } }
//!
//! The suggestion field is the point: it tells a calling LLM how to fix its own
//! call (e.g. "call list_services first"). Every constructor here fills it in.

use serde::Serialize;
use serde_json::Value;

/// Stable, machine-readable failure codes. Part of the public contract:
/// agents may branch on them, so they must never be renamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Unauthorized,
    ForbiddenScope,
    RateLimited,
    NotFound,
    MethodNotAllowed,
    InvalidParameter,
    UnknownTool,
    UnknownResource,
    UnknownPrompt,
    UnknownServiceId,
    InvalidUrl,
    TlsBypassRefused,
    ServiceDisabled,
    InvalidNavAction,
    ConfigInvalid,
    ConfigTooLarge,
    ConfirmationRequired,
    Internal,
    NotRunning,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Unauthorized => "UNAUTHORIZED",
            ErrorCode::ForbiddenScope => "FORBIDDEN_SCOPE",
            ErrorCode::RateLimited => "RATE_LIMITED",
            ErrorCode::NotFound => "NOT_FOUND",
            ErrorCode::MethodNotAllowed => "METHOD_NOT_ALLOWED",
            ErrorCode::InvalidParameter => "INVALID_PARAMETER",
            ErrorCode::UnknownTool => "UNKNOWN_TOOL",
            ErrorCode::UnknownResource => "UNKNOWN_RESOURCE",
            ErrorCode::UnknownPrompt => "UNKNOWN_PROMPT",
            ErrorCode::UnknownServiceId => "UNKNOWN_SERVICE_ID",
            ErrorCode::InvalidUrl => "INVALID_URL",
            ErrorCode::TlsBypassRefused => "TLS_BYPASS_REFUSED",
            ErrorCode::ServiceDisabled => "SERVICE_DISABLED",
            ErrorCode::InvalidNavAction => "INVALID_NAV_ACTION",
            ErrorCode::ConfigInvalid => "CONFIG_INVALID",
            ErrorCode::ConfigTooLarge => "CONFIG_TOO_LARGE",
            ErrorCode::ConfirmationRequired => "CONFIRMATION_REQUIRED",
            ErrorCode::Internal => "INTERNAL_ERROR",
            ErrorCode::NotRunning => "BZDIUM_NOT_RUNNING",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentError {
    pub code: &'static str,
    pub message: String,
    pub suggestion: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl AgentError {
    pub fn new(code: ErrorCode, message: impl Into<String>, suggestion: impl Into<String>) -> Self {
        Self {
            code: code.as_str(),
            message: message.into(),
            suggestion: suggestion.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    /// HTTP status for the REST projection of this error.
    pub fn status(&self) -> u16 {
        match self.code {
            "UNAUTHORIZED" => 401,
            "FORBIDDEN_SCOPE" => 403,
            "RATE_LIMITED" => 429,
            "NOT_FOUND" | "UNKNOWN_TOOL" | "UNKNOWN_RESOURCE" | "UNKNOWN_PROMPT" => 404,
            "METHOD_NOT_ALLOWED" => 405,
            "INTERNAL_ERROR" | "BZDIUM_NOT_RUNNING" => 500,
            _ => 400,
        }
    }

    pub fn invalid_parameter(
        field: &str,
        message: impl Into<String>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self::new(ErrorCode::InvalidParameter, message, suggestion)
            .with_details(serde_json::json!({ "field": field }))
    }

    /// Classifies a command-layer Err(String) into an agent error carrying a
    /// corrective suggestion. This is the single translation point between
    /// the 22 existing #[tauri::command] functions and the agent contract,
    /// so both transports stay consistent and domain wording is never lost.
    pub fn from_command(message: String) -> Self {
        let m = message.as_str();

        if let Some(id) = m.strip_prefix("unknown service id ") {
            return Self::new(
                ErrorCode::UnknownServiceId,
                format!("No service is configured with id {id}."),
                "Call list_services to retrieve valid service ids, then retry with one of them.",
            )
            .with_details(serde_json::json!({ "received": id.trim() }));
        }
        if m == "only https:// URLs are allowed" {
            return Self::new(
                ErrorCode::InvalidUrl,
                "The URL scheme must be https.",
                "Supply a full https URL such as https://web.whatsapp.com.",
            );
        }
        if m.starts_with("invalid URL") {
            return Self::new(
                ErrorCode::InvalidUrl,
                message.clone(),
                "Supply an absolute https URL, e.g. https://app.example.com.",
            );
        }
        if m.starts_with("TLS verification can only be disabled") {
            return Self::new(
                ErrorCode::TlsBypassRefused,
                message.clone(),
                "This host is a built-in public service, so ignoreTlsErrors is refused by policy. Remove ignoreTlsErrors from the request; it is only permitted for self-hosted hosts.",
            );
        }
        if m.contains("is disabled") && m.contains("enable it") {
            return Self::new(
                ErrorCode::ServiceDisabled,
                message.clone(),
                "Enable the service first with update_service { enabled: true }, then retry.",
            );
        }
        if m == "developer mode is disabled" {
            return Self::new(
                ErrorCode::ForbiddenScope,
                "Developer mode is disabled, so DevTools cannot be opened.",
                "DevTools are intentionally not exposed to agents. A human must enable Developer mode in bzdium Settings.",
            );
        }
        if m.starts_with("unknown navigate action ") {
            return Self::new(
                ErrorCode::InvalidNavAction,
                message.clone(),
                "Use one of the supported actions: back, forward, or home.",
            );
        }
        // Must be checked before the generic config cases: a missing or
        // expired confirmation token is an authorisation precondition, not a
        // malformed document, and the suggestion is completely different.
        if m.starts_with("confirmation token is missing or expired") {
            return Self::new(
                ErrorCode::ConfirmationRequired,
                message.clone(),
                "Call export_config first: it returns a confirmationToken valid for 10 minutes. Then retry import_config with that token.",
            );
        }
        if m.starts_with("not a bzdium config file") {
            return Self::new(
                ErrorCode::ConfigInvalid,
                message.clone(),
                "The payload must be a bzdium config document with type bzdium-config and version 1. Call export_config to obtain a valid example.",
            );
        }
        if m.starts_with("unsupported config file") {
            return Self::new(
                ErrorCode::ConfigInvalid,
                message.clone(),
                "Only config documents with type bzdium-config and version 1 are accepted.",
            );
        }
        if m.starts_with("config file is too large") {
            return Self::new(
                ErrorCode::ConfigTooLarge,
                message.clone(),
                "Reduce the number of services in the config; the limit is 2 MiB.",
            );
        }
        if m.contains("service name must not be empty") {
            return Self::new(
                ErrorCode::InvalidParameter,
                "The service name must not be empty.",
                "Provide a non-empty name, e.g. Work Slack.",
            );
        }
        Self::new(
            ErrorCode::Internal,
            message,
            "This is an unexpected failure inside bzdium. Retry once; if it persists, inspect bzdium://audit/recent.",
        )
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AgentError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_carries_a_suggestion() {
        let samples = vec![
            AgentError::from_command("unknown service id abc".into()),
            AgentError::from_command("only https:// URLs are allowed".into()),
            AgentError::from_command("boom".into()),
            AgentError::new(ErrorCode::Unauthorized, "no token", "send a bearer token"),
        ];
        for e in samples {
            assert!(!e.message.is_empty(), "message must not be empty");
            assert!(!e.suggestion.is_empty(), "suggestion must not be empty");
            assert!(e.suggestion.len() > 15, "suggestion must be actionable");
        }
    }

    #[test]
    fn unknown_service_id_is_classified_with_details() {
        let e = AgentError::from_command("unknown service id u-1".into());
        assert_eq!(e.code, "UNKNOWN_SERVICE_ID");
        assert_eq!(e.status(), 400);
        assert_eq!(e.details.unwrap()["received"], "u-1");
    }

    #[test]
    fn tls_refusal_is_classified() {
        let e = AgentError::from_command(
            "TLS verification can only be disabled for self-hosted services".into(),
        );
        assert_eq!(e.code, "TLS_BYPASS_REFUSED");
        assert!(e.suggestion.contains("self-hosted"));
    }

    #[test]
    fn status_mapping_is_total_and_sane() {
        assert_eq!(AgentError::new(ErrorCode::Unauthorized, "a", "b").status(), 401);
        assert_eq!(AgentError::new(ErrorCode::ForbiddenScope, "a", "b").status(), 403);
        assert_eq!(AgentError::new(ErrorCode::UnknownTool, "a", "b").status(), 404);
        assert_eq!(AgentError::new(ErrorCode::Internal, "a", "b").status(), 500);
        assert_eq!(AgentError::new(ErrorCode::InvalidUrl, "a", "b").status(), 400);
    }

    #[test]
    fn serialises_to_prd_envelope() {
        let v = serde_json::to_value(AgentError::new(ErrorCode::UnknownTool, "m", "s")).unwrap();
        assert_eq!(v["code"], "UNKNOWN_TOOL");
        assert_eq!(v["message"], "m");
        assert_eq!(v["suggestion"], "s");
        // details omitted when absent
        assert!(v.get("details").is_none());
    }
}
