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

    // The published schemas promise `additionalProperties: false` and
    // required-field enforcement; enforce it here so a typo'd key (e.g.
    // `dryrun`) is rejected loudly instead of silently running a mutation
    // with defaults, and unknown keys can never masquerade as success.
    let schema = (spec.input_schema)();
    validate_against_schema(&spec.name, &schema, &args)?;

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

/// Validates one value against one property schema (the subset the registry
/// emits: string/boolean/integer/array-of-string, enum, anyOf).
fn value_matches(prop: &Value, value: &Value) -> bool {
    if let Some(branches) = prop.get("anyOf").and_then(Value::as_array) {
        return branches.iter().any(|b| value_matches(b, value));
    }
    match prop.get("type").and_then(Value::as_str) {
        Some("string") => {
            if !value.is_string() {
                return false;
            }
            match prop.get("enum").and_then(Value::as_array) {
                Some(allowed) => allowed.contains(value),
                None => true,
            }
        }
        Some("boolean") => value.is_boolean(),
        Some("integer") => value.is_i64() || value.is_u64(),
        Some("number") => value.is_number(),
        Some("array") => value.as_array().is_some_and(|items| {
            items
                .iter()
                .all(|item| value_matches(prop.get("items").unwrap_or(&json!({})), item))
        }),
        _ => true,
    }
}

/// Enforces the object schemas built by `schema::object`: unknown keys are
/// rejected when `additionalProperties` is false, `required` keys must be
/// present, and declared types/enums are checked. Anything the subset does
/// not express is left to the tool's own validators.
fn validate_against_schema(tool: &str, schema: &Value, args: &Value) -> Result<(), AgentError> {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Ok(()); // not an object schema — nothing to enforce here
    };
    let obj = args.as_object().expect("dispatch already checked for object");
    let additional_ok = schema
        .get("additionalProperties")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    for (key, value) in obj {
        match properties.get(key) {
            Some(prop) => {
                if !value_matches(prop, value) {
                    return Err(AgentError::invalid_parameter(
                        key,
                        format!("'{key}' does not match its declared type in tool '{tool}'."),
                        "Call tools/list and match the declared property types exactly.",
                    ));
                }
            }
            None if !additional_ok => {
                return Err(AgentError::invalid_parameter(
                    key,
                    format!("Unknown argument '{key}' for tool '{tool}'."),
                    "Remove it, or call tools/list to see the exact accepted arguments — a typo'd key is never silently ignored.",
                ));
            }
            None => {}
        }
    }
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            if !obj.contains_key(name) {
                return Err(AgentError::invalid_parameter(
                    name,
                    format!("Missing required argument '{name}' for tool '{tool}'."),
                    "Supply every required argument; the schema lists them under `required`.",
                ));
            }
        }
    }
    Ok(())
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

    fn sample_schema() -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["id"],
            "properties": {
                "id": { "type": "string", "description": "service id" },
                "count": { "type": "integer", "description": "how many" },
                "linkTarget": { "type": "string", "enum": ["browser", "bzdium"], "description": "target" },
                "ids": { "type": "array", "items": { "type": "string" }, "description": "list" },
                "pin": {
                    "anyOf": [{ "type": "string" }, { "type": "null" }],
                    "description": "optional pin"
                }
            }
        })
    }

    #[test]
    fn schema_rejects_unknown_keys() {
        // A typo'd key must never silently run a mutation with defaults.
        let err = validate_against_schema("t", &sample_schema(), &json!({ "id": "x", "dryrun": true }));
        assert!(err.is_err());
        assert!(err.unwrap_err().message.contains("dryrun"));
    }

    #[test]
    fn schema_rejects_missing_required() {
        let err = validate_against_schema("t", &sample_schema(), &json!({ "count": 1 }));
        assert!(err.is_err());
    }

    #[test]
    fn schema_rejects_type_and_enum_mismatches() {
        let s = sample_schema();
        assert!(validate_against_schema("t", &s, &json!({ "id": 5 })).is_err(), "integer for string");
        assert!(validate_against_schema("t", &s, &json!({ "id": "x", "count": "3" })).is_err(), "string for integer");
        assert!(validate_against_schema("t", &s, &json!({ "id": "x", "linkTarget": "safari" })).is_err(), "off-enum");
        assert!(validate_against_schema("t", &s, &json!({ "id": "x", "ids": [1, 2] })).is_err(), "non-string items");
    }

    #[test]
    fn schema_accepts_valid_and_anyof_null() {
        let s = sample_schema();
        assert!(validate_against_schema("t", &s, &json!({ "id": "x" })).is_ok());
        assert!(validate_against_schema("t", &s, &json!({ "id": "x", "count": 3, "linkTarget": "browser", "ids": ["a"] })).is_ok());
        assert!(validate_against_schema("t", &s, &json!({ "id": "x", "pin": null })).is_ok());
        assert!(validate_against_schema("t", &s, &json!({ "id": "x", "pin": "C:/rt" })).is_ok());
    }

    #[test]
    fn schema_without_properties_is_skipped() {
        assert!(validate_against_schema("t", &json!({ "type": "object" }), &json!({ "x": 1 })).is_ok());
    }

    #[test]
    fn every_registered_tool_schema_passes_its_own_validator() {
        // The registry's schemas must be expressible in the enforced subset;
        // this drift-guards schema helpers against the validator.
        for spec in super::super::tools::all_tools() {
            let schema = (spec.input_schema)();
            // A no-arg call must either validate or fail ONLY on `required`.
            let probe = json!({});
            if let Err(e) = validate_against_schema(spec.name, &schema, &probe) {
                assert!(
                    e.message.contains("required") || e.message.contains("Missing"),
                    "tool {} rejects an empty probe for a non-required reason: {}",
                    spec.name,
                    e.message
                );
            }
        }
    }
}
