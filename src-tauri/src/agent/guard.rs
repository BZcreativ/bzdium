//! Dry-run support and dispatch.
//!
//! A mutating tool cannot be safely "run and rolled back": creating a webview
//! or deleting a session profile is not transactional. So instead of undoing
//! work, each mutating tool optionally declares a PREVIEW handler that runs the
//! same validators and returns the effect it would have, touching nothing.
//!
//! This is what makes dryRun honest rather than decorative: the preview path
//! shares validate_https_url, the TLS-bypass policy and the id checks with the
//! real path, so a preview that succeeds tells the agent the real call will
//! pass validation.

use serde_json::{json, Value};

use super::error::{AgentError, ErrorCode};
use super::tools::find_tool;
use super::{AgentCtx, ToolSpec};

/// Outcome of a dispatch, so callers can audit dry runs distinctly.
pub struct Dispatched {
    pub value: Value,
    pub dry_run: bool,
}

/// Resolves and executes a tool call.
///
/// Scope checking happens in the transport (which knows the caller's token);
/// this function assumes authorization already passed.
pub async fn dispatch(
    ctx: &AgentCtx,
    name: &str,
    args: Value,
    request_dry_run: bool,
) -> Result<Dispatched, AgentError> {
    let spec = find_tool(name).ok_or_else(|| super::unknown_tool(name))?;

    if !args.is_object() && !args.is_null() {
        return Err(AgentError::invalid_parameter(
            "arguments",
            "The arguments value must be a JSON object.",
            "Send a JSON object, e.g. {\"id\": \"...\"}. Call tools/list for each tool's schema.",
        ));
    }
    let args = if args.is_null() { json!({}) } else { args };

    // A non-mutating tool cannot meaningfully be dry-run; ignore the flag
    // rather than erroring, so agents can pass it uniformly.
    let dry_run = request_dry_run && spec.mutating;

    if dry_run {
        let Some(preview) = spec.preview else {
            return Err(AgentError::new(
                ErrorCode::InvalidParameter,
                format!("Tool '{}' does not support dryRun.", spec.name),
                "Re-issue the call without dryRun, or call it with dryRun: false once you are ready.",
            ));
        };
        let projected = preview(ctx, args).await?;
        return Ok(Dispatched {
            value: json!({
                "dryRun": true,
                "tool": spec.name,
                "applied": false,
                "wouldApply": projected,
                "note": "Nothing was changed. Re-issue with dryRun: false to commit."
            }),
            dry_run: true,
        });
    }

    let value = (spec.handler)(ctx, args).await?;
    Ok(Dispatched { value, dry_run: false })
}

/// Reads the dryRun flag from raw arguments before validation.
pub fn wants_dry_run(args: &Value) -> bool {
    args.get("dryRun").and_then(Value::as_bool).unwrap_or(false)
}

/// Removes dryRun so it is never mistaken for a tool parameter.
pub fn strip_dry_run(mut args: Value) -> Value {
    if let Some(obj) = args.as_object_mut() {
        obj.remove("dryRun");
    }
    args
}

/// Convenience for the preview handlers: a projected snapshot of the services
/// list as it would look after the change, without applying it.
pub fn project_service_list(ctx: &AgentCtx, mutate: impl FnOnce(&mut Vec<crate::state::Service>)) -> Value {
    use tauri::Manager as _;
    let (mut services, active) = {
        let state = ctx.app.state::<crate::state::SharedState>();
        let inner = state.lock().expect("state mutex poisoned");
        (inner.services.clone(), inner.active_service_id.clone())
    };
    mutate(&mut services);
    services.sort_by_key(|s| s.order);
    json!({
        "services": services,
        "activeServiceId": active,
        "count": services.len(),
    })
}

/// True when a spec supports dry running.
pub fn supports_dry_run(spec: &ToolSpec) -> bool {
    spec.mutating && spec.preview.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wants_dry_run_reads_boolean() {
        assert!(wants_dry_run(&json!({ "dryRun": true })));
        assert!(!wants_dry_run(&json!({ "dryRun": false })));
        assert!(!wants_dry_run(&json!({})));
        // A non-boolean must not be coerced.
        assert!(!wants_dry_run(&json!({ "dryRun": "yes" })));
    }

    #[test]
    fn strip_dry_run_removes_only_that_key() {
        let v = strip_dry_run(json!({ "id": "x", "dryRun": true }));
        assert!(v.get("dryRun").is_none());
        assert_eq!(v["id"], "x");
    }

    #[test]
    fn strip_dry_run_handles_non_object() {
        let v = strip_dry_run(json!(null));
        assert!(v.is_null());
    }
}
