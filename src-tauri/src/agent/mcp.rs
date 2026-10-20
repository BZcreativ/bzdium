//! Model Context Protocol, implemented directly against the specification.
//!
//! No rmcp: the protocol surface a server must speak is small and stable, and
//! the repo's identity is a minimal dependency tree. Implementing it here also
//! keeps ONE dispatch path -- MCP tools/call, REST POST /tools/{name} and the
//! stdio bridge all reach guard::dispatch, so scope checks and dry runs cannot
//! diverge between transports.
//!
//! Framing: JSON-RPC 2.0. Over HTTP the transport is streamable HTTP with a
//! single JSON response; over stdio it is newline-delimited JSON, handled by
//! stdio.rs. Batch arrays are supported in both directions, and a notification
//! (a request with no id) produces no response object.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};

use super::error::{AgentError, ErrorCode};
use super::guard::{dispatch, strip_dry_run, wants_dry_run};
use super::prompts::{find_prompt, PROMPTS};
use super::resources::{find_resource, RESOURCES};
use super::scope::{require_scope, Scope};
use super::tools::{find_tool, tools_schema_list};
use super::AgentCtx;

/// JSON-RPC 2.0 and MCP error codes. The reserved range is -32768..-32000;
/// implementation-defined server errors live at -32000..-32099.
pub mod code {
    /// Invalid JSON was received by the server.
    pub const PARSE_ERROR: i64 = -32700;
    /// The JSON sent is not a valid Request object.
    pub const INVALID_REQUEST: i64 = -32600;
    /// The method does not exist or is not available.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid method parameters.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal JSON-RPC error.
    pub const INTERNAL_ERROR: i64 = -32603;
    /// Not authenticated. Implementation-defined.
    pub const UNAUTHORIZED: i64 = -32001;
    /// Authenticated but under-scoped. Implementation-defined.
    pub const FORBIDDEN_SCOPE: i64 = -32002;
    /// Rate limited. Implementation-defined.
    pub const RATE_LIMITED: i64 = -32003;
    /// A tool, resource or prompt was not found. Implementation-defined.
    pub const NOT_FOUND: i64 = -32004;
}

/// Protocol revisions this server can speak, newest first.
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// Methods this server dispatches. Used for a clear METHOD_NOT_FOUND message
/// and by the tests to prove the advertised capabilities match reality.
pub const METHODS: [&str; 10] = [
    "initialize",
    "notifications/initialized",
    "ping",
    "tools/list",
    "tools/call",
    "resources/list",
    "resources/templates/list",
    "resources/read",
    "prompts/list",
    "prompts/get",
];

/// Maps an AgentError onto a JSON-RPC error code.
///
/// Authorization failures are protocol errors because a client must be able to
/// tell "I am not allowed to call this" apart from "the call failed". Tool
/// execution failures are reported as an isError tool RESULT instead, per the
/// MCP specification, so the model sees them in-band.
pub fn rpc_code_for(err: &AgentError) -> i64 {
    match err.code {
        "UNAUTHORIZED" => code::UNAUTHORIZED,
        "FORBIDDEN_SCOPE" => code::FORBIDDEN_SCOPE,
        "RATE_LIMITED" => code::RATE_LIMITED,
        "UNKNOWN_TOOL" | "UNKNOWN_RESOURCE" | "UNKNOWN_PROMPT" | "NOT_FOUND" => code::NOT_FOUND,
        "INVALID_PARAMETER" => code::INVALID_PARAMS,
        _ => code::INTERNAL_ERROR,
    }
}

/// True for failures a client cannot fix by changing arguments, which are
/// therefore protocol-level errors rather than in-band tool errors.
pub fn is_protocol_failure(err: &AgentError) -> bool {
    matches!(err.code, "UNAUTHORIZED" | "FORBIDDEN_SCOPE" | "RATE_LIMITED")
}

/// Builds a JSON-RPC error object carrying the agent chrome under data.
pub fn error_object(id: Value, rpc_code: i64, err: &AgentError) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": rpc_code,
            "message": err.message.clone(),
            "data": {
                "agentCode": err.code,
                "suggestion": err.suggestion,
                "details": err.details,
                "httpStatus": err.status(),
            }
        }
    })
}

fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// The server identity block returned by initialize.
pub fn server_info() -> Value {
    json!({
        "name": "bzdium",
        "version": env!("CARGO_PKG_VERSION"),
        "title": "bzdium desktop control",
    })
}

/// Capabilities this server actually implements. Declaring a capability the
/// server does not honour would make a conforming client hang, so this list is
/// kept in step with dispatch_method below.
pub fn capabilities() -> Value {
    json!({
        "tools": { "listChanged": false },
        "resources": { "subscribe": false, "listChanged": false },
        "prompts": { "listChanged": false },
    })
}

/// Picks the protocol revision to answer with: the client's if we know it,
/// otherwise the newest we implement.
pub fn negotiate_version(requested: Option<&str>) -> &'static str {
    match requested {
        Some(v) if SUPPORTED_PROTOCOL_VERSIONS.contains(&v) => SUPPORTED_PROTOCOL_VERSIONS
            .iter()
            .find(|s| **s == v)
            .copied()
            .unwrap_or(SUPPORTED_PROTOCOL_VERSIONS[0]),
        _ => SUPPORTED_PROTOCOL_VERSIONS[0],
    }
}

fn method_not_found(id: Value, method: &str) -> Value {
    error_object(
        id,
        code::METHOD_NOT_FOUND,
        &AgentError::new(
            ErrorCode::NotFound,
            format!("The MCP method '{method}' is not implemented by this server."),
            format!("Supported methods: {}.", METHODS.join(", ")),
        )
        .with_details(json!({ "method": method, "supported": METHODS })),
    )
}

/// A shared empty map, so the common "no params" case needs no allocation.
fn empty_map() -> &'static serde_json::Map<String, Value> {
    use std::sync::OnceLock;
    static EMPTY: OnceLock<serde_json::Map<String, Value>> = OnceLock::new();
    EMPTY.get_or_init(serde_json::Map::new)
}

fn params_object(params: Option<&Value>) -> Result<&serde_json::Map<String, Value>, AgentError> {
    match params {
        None | Some(Value::Null) => Ok(empty_map()),
        Some(Value::Object(map)) => Ok(map),
        Some(_) => Err(AgentError::invalid_parameter(
            "params",
            "The params member must be a JSON object.",
            "Send params as an object, for example a name and an arguments map.",
        )),
    }
}

fn required_str<'a>(
    params: &'a serde_json::Map<String, Value>,
    key: &str,
    method: &str,
) -> Result<&'a str, AgentError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            AgentError::invalid_parameter(
                key,
                format!("The '{key}' parameter is required and must be a non-empty string."),
                match method {
                    "tools/call" => "Call tools/list for the registered tool names, then pass a name plus an arguments object.",
                    "resources/read" => "Call resources/list or resources/templates/list for valid URIs, then pass uri.",
                    "prompts/get" => "Call prompts/list for the registered prompt names, then pass name.",
                    _ => "Supply a non-empty string.",
                },
            )
            .with_details(json!({ "parameter": key, "method": method }))
        })
}

/// The MCP tools/list payload, projected from the one registry table.
pub fn tools_list() -> Value {
    let tools: Vec<Value> = tools_schema_list()
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let name = row.get("name")?.as_str()?;
                    let mutating = row.get("mutating").and_then(Value::as_bool).unwrap_or(false);
                    // MCP tools carry name, description and inputSchema; the
                    // scope and mutating flags are extras real clients ignore
                    // but agents read.
                    Some(json!({
                        "name": name,
                        "description": row.get("description").cloned().unwrap_or(Value::Null),
                        "inputSchema": row.get("inputSchema").cloned()
                            .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                        "annotations": {
                            "title": name,
                            "readOnlyHint": !mutating,
                            "destructiveHint": mutating,
                        },
                        "x-bzdium-scope": row.get("scope").cloned().unwrap_or(Value::Null),
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    json!({ "tools": tools })
}

/// The MCP resources/list payload: the static resources.
pub fn resources_list() -> Value {
    let resources: Vec<Value> = RESOURCES
        .iter()
        .filter(|r| !r.uri_template.contains('{'))
        .map(|r| {
            json!({
                "uri": r.uri_template,
                "name": r.name,
                "description": r.description,
                "mimeType": r.mime_type,
                "x-bzdium-scope": r.scope.as_str(),
            })
        })
        .collect();
    json!({ "resources": resources })
}

/// The MCP resources/templates/list payload: the parameterised resources.
pub fn resource_templates_list() -> Value {
    let templates: Vec<Value> = RESOURCES
        .iter()
        .filter(|r| r.uri_template.contains('{'))
        .map(|r| {
            json!({
                "uriTemplate": r.uri_template,
                "name": r.name,
                "description": r.description,
                "mimeType": r.mime_type,
                "x-bzdium-scope": r.scope.as_str(),
            })
        })
        .collect();
    json!({ "resourceTemplates": templates })
}

/// The MCP prompts/list payload.
pub fn prompts_list() -> Value {
    let prompts: Vec<Value> = PROMPTS
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "description": p.description,
                "arguments": (p.arguments)(),
                "x-bzdium-scope": p.scope.as_str(),
            })
        })
        .collect();
    json!({ "prompts": prompts })
}

/// Validates the tools/call arguments member and lifts the dryRun flag out of
/// it, so the tool never sees a parameter it did not declare.
pub fn tool_call_arguments(
    params: &serde_json::Map<String, Value>,
) -> Result<(Value, bool), AgentError> {
    let raw = match params.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(v @ Value::Object(_)) => v.clone(),
        Some(_) => {
            return Err(AgentError::invalid_parameter(
                "arguments",
                "The 'arguments' member must be a JSON object.",
                "Send a name plus an arguments object. Call tools/list for each tool's inputSchema.",
            ))
        }
    };
    let dry_run = wants_dry_run(&raw);
    Ok((strip_dry_run(raw), dry_run))
}

/// Wraps a successful tool result in the MCP content envelope.
///
/// The value is provided BOTH as rendered text and as structuredContent: text
/// for clients that only read content blocks, structured for clients that can
/// use the shape directly.
pub fn tool_result(value: Value) -> Value {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": value,
        "isError": false,
    })
}

/// Wraps a tool execution failure as an in-band MCP error result.
pub fn tool_error_result(err: &AgentError) -> Value {
    let envelope = serde_json::to_value(err).unwrap_or_else(|_| {
        json!({ "code": "INTERNAL_ERROR", "message": err.message, "suggestion": err.suggestion })
    });
    let text = serde_json::to_string_pretty(&envelope).unwrap_or_default();
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": envelope,
        "isError": true,
    })
}

/// Renders a prompt as the single user message MCP expects.
pub fn prompt_result(spec_name: &str, description: &str, text: String) -> Value {
    json!({
        "description": description,
        "messages": [{
            "role": "user",
            "content": { "type": "text", "text": text }
        }],
        "x-bzdium-prompt": spec_name,
    })
}

/// Dispatches one JSON-RPC request object.
///
/// Returns None for a notification, which by specification has no response.
async fn dispatch_method(
    ctx: &Arc<AgentCtx>,
    granted: Scope,
    actor: &str,
    id: Value,
    method: &str,
    params: Option<&Value>,
) -> Option<Value> {
    // Notifications never produce a response, even on failure.
    let is_notification = id.is_null();

    match method {
        "initialize" => {
            if is_notification {
                return None;
            }
            let requested = params
                .and_then(|p| p.get("protocolVersion"))
                .and_then(Value::as_str);
            Some(success(
                id,
                json!({
                    "protocolVersion": negotiate_version(requested),
                    "capabilities": capabilities(),
                    "serverInfo": server_info(),
                    // Informational, outside the spec's required set: tells an
                    // agent the auth model it is already operating under.
                    "instructions": format!(
                        "You are connected to the bzdium desktop app as token '{actor}' with scope '{granted}'. \
                         Call tools/list first, then read resources such as bzdium://app/state before acting. \
                         Mutating tools accept dryRun: true to preview their effect without committing."
                    ),
                }),
            ))
        }

        // The client acknowledges initialization. Nothing to do, nothing to say.
        "notifications/initialized" | "notifications/cancelled" => None,

        "ping" => {
            if is_notification {
                return None;
            }
            Some(success(id, json!({})))
        }

        "tools/list" => {
            if is_notification {
                return None;
            }
            Some(success(id, tools_list()))
        }

        "tools/call" => {
            if is_notification {
                return None;
            }
            let map = match params_object(params) {
                Ok(m) => m,
                Err(err) => return Some(error_object(id, code::INVALID_PARAMS, &err)),
            };
            let name = match required_str(map, "name", "tools/call") {
                Ok(n) => n.to_string(),
                Err(err) => return Some(error_object(id, code::INVALID_PARAMS, &err)),
            };
            let (args, dry_run) = match tool_call_arguments(map) {
                Ok(pair) => pair,
                Err(err) => return Some(error_object(id, code::INVALID_PARAMS, &err)),
            };

            // An unknown tool is a protocol-level error: the request cannot be
            // corrected by changing arguments.
            let Some(spec) = find_tool(&name) else {
                let err = super::unknown_tool(&name);
                return Some(error_object(id, code::NOT_FOUND, &err));
            };
            // Scope is enforced HERE, in the transport, exactly as the REST
            // route does, and BEFORE the handler runs. tools/call and
            // POST /tools/{name} therefore cannot diverge.
            if let Err(err) = require_scope(granted, spec.scope, spec.name) {
                return Some(error_object(id, rpc_code_for(&err), &err));
            }

            match dispatch(ctx, &name, args.clone(), dry_run).await {
                // dispatch() is the single shared implementation of scope
                // enforcement and dry-run preview. The audit entry is written
                // by the HTTP layer that owns the request, so REST and
                // MCP-over-HTTP land in one log; a stdio caller forwards over
                // HTTP and is therefore audited by that same code path.
                Ok(dispatched) => Some(success(id, tool_result(dispatched.value))),
                Err(err) if is_protocol_failure(&err) => {
                    Some(error_object(id, rpc_code_for(&err), &err))
                }
                Err(err) => Some(success(id, tool_error_result(&err))),
            }
        }

        "resources/list" => {
            if is_notification {
                return None;
            }
            Some(success(id, resources_list()))
        }

        "resources/templates/list" => {
            if is_notification {
                return None;
            }
            Some(success(id, resource_templates_list()))
        }

        "resources/read" => {
            if is_notification {
                return None;
            }
            let map = match params_object(params) {
                Ok(m) => m,
                Err(err) => return Some(error_object(id, code::INVALID_PARAMS, &err)),
            };
            let uri = match required_str(map, "uri", "resources/read") {
                Ok(u) => u.to_string(),
                Err(err) => return Some(error_object(id, code::INVALID_PARAMS, &err)),
            };
            let Some((spec, resource_params)) = find_resource(&uri) else {
                let err = AgentError::new(
                    ErrorCode::UnknownResource,
                    format!("No resource matches '{uri}'."),
                    "Call resources/list and resources/templates/list for the valid URIs.",
                )
                .with_details(json!({ "uri": uri }));
                return Some(error_object(id, code::NOT_FOUND, &err));
            };
            if let Err(err) = require_scope(granted, spec.scope, spec.name) {
                return Some(error_object(id, rpc_code_for(&err), &err));
            }
            match (spec.render)(ctx, &resource_params).await {
                Ok(text) => Some(success(
                    id,
                    json!({
                        "contents": [{
                            "uri": uri,
                            "mimeType": spec.mime_type,
                            "text": text,
                        }]
                    }),
                )),
                Err(err) => Some(error_object(id, rpc_code_for(&err), &err)),
            }
        }

        "prompts/list" => {
            if is_notification {
                return None;
            }
            Some(success(id, prompts_list()))
        }

        "prompts/get" => {
            if is_notification {
                return None;
            }
            let map = match params_object(params) {
                Ok(m) => m,
                Err(err) => return Some(error_object(id, code::INVALID_PARAMS, &err)),
            };
            let name = match required_str(map, "name", "prompts/get") {
                Ok(n) => n.to_string(),
                Err(err) => return Some(error_object(id, code::INVALID_PARAMS, &err)),
            };
            let Some(spec) = find_prompt(&name) else {
                let err = AgentError::new(
                    ErrorCode::UnknownPrompt,
                    format!("No prompt named '{name}' is registered."),
                    "Call prompts/list for the registered prompt names.",
                )
                .with_details(json!({ "prompt": name }));
                return Some(error_object(id, code::NOT_FOUND, &err));
            };
            if let Err(err) = require_scope(granted, spec.scope, spec.name) {
                return Some(error_object(id, rpc_code_for(&err), &err));
            }
            let mut args: HashMap<String, String> = HashMap::new();
            // MCP prompt arguments are a string map; coerce scalars so a client
            // sending a number where a string is expected still works.
            if let Some(Value::Object(map)) = map.get("arguments") {
                for (k, v) in map {
                    match v {
                        Value::String(s) => {
                            args.insert(k.clone(), s.clone());
                        }
                        Value::Null => {}
                        other => {
                            args.insert(k.clone(), other.to_string());
                        }
                    }
                }
            }
            let text = (spec.render)(&args);
            Some(success(id, prompt_result(spec.name, spec.description, text)))
        }

        other => {
            if is_notification {
                return None;
            }
            Some(method_not_found(id, other))
        }
    }
}

/// Validates one JSON-RPC envelope. Returns (id, method, params) or a ready
/// error response for a malformed object.
fn read_envelope(value: &Value) -> Result<(Value, String, Option<Value>), Value> {
    let Some(obj) = value.as_object() else {
        return Err(error_object(
            Value::Null,
            code::INVALID_REQUEST,
            &AgentError::new(
                ErrorCode::InvalidParameter,
                "A JSON-RPC request must be a JSON object.",
                "Send a jsonrpc 2.0 request object with an id and a method.",
            ),
        ));
    };
    // The id is echoed even when the rest of the envelope is invalid, so a
    // client can correlate the failure with its own request.
    let id = obj.get("id").cloned().unwrap_or(Value::Null);
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(error_object(
            id,
            code::INVALID_REQUEST,
            &AgentError::new(
                ErrorCode::InvalidParameter,
                "The 'jsonrpc' member must be exactly the string '2.0'.",
                "Every request needs a jsonrpc member set to 2.0.",
            ),
        ));
    }
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return Err(error_object(
            id,
            code::INVALID_REQUEST,
            &AgentError::new(
                ErrorCode::InvalidParameter,
                "The 'method' member is required and must be a string.",
                format!("Supported methods: {}.", METHODS.join(", ")),
            ),
        ));
    };
    Ok((id, method.to_string(), obj.get("params").cloned()))
}

/// Handles a complete JSON-RPC payload: one request object, or a batch array.
///
/// Returns Value::Null when the payload contains only notifications, which the
/// HTTP layer turns into an empty 202 response.
pub async fn handle(ctx: Arc<AgentCtx>, granted: Scope, actor: String, payload: Value) -> Value {
    match payload {
        Value::Array(items) => {
            if items.is_empty() {
                return error_object(
                    Value::Null,
                    code::INVALID_REQUEST,
                    &AgentError::new(
                        ErrorCode::InvalidParameter,
                        "An empty batch is not a valid JSON-RPC request.",
                        "Send at least one request object, or omit the array.",
                    ),
                );
            }
            let mut responses = Vec::with_capacity(items.len());
            for item in &items {
                if let Some(response) = handle_one(&ctx, granted, &actor, item).await {
                    responses.push(response);
                }
            }
            if responses.is_empty() {
                Value::Null
            } else {
                Value::Array(responses)
            }
        }
        other => handle_one(&ctx, granted, &actor, &other)
            .await
            .unwrap_or(Value::Null),
    }
}

async fn handle_one(
    ctx: &Arc<AgentCtx>,
    granted: Scope,
    actor: &str,
    item: &Value,
) -> Option<Value> {
    match read_envelope(item) {
        Ok((id, method, params)) => {
            dispatch_method(ctx, granted, actor, id, &method, params.as_ref()).await
        }
        Err(response) => Some(response),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tools::all_tools;

    #[test]
    fn json_rpc_error_codes_match_the_specification() {
        assert_eq!(code::PARSE_ERROR, -32700);
        assert_eq!(code::INVALID_REQUEST, -32600);
        assert_eq!(code::METHOD_NOT_FOUND, -32601);
        assert_eq!(code::INVALID_PARAMS, -32602);
        assert_eq!(code::INTERNAL_ERROR, -32603);
        for c in [code::UNAUTHORIZED, code::FORBIDDEN_SCOPE, code::RATE_LIMITED, code::NOT_FOUND] {
            assert!((-32099..=-32000).contains(&c), "{c} is outside the server error range");
        }
    }

    #[test]
    fn error_object_carries_agent_chrome() {
        let err = AgentError::new(ErrorCode::ForbiddenScope, "nope", "use an admin token");
        let obj = error_object(json!(7), rpc_code_for(&err), &err);
        assert_eq!(obj["jsonrpc"], "2.0");
        assert_eq!(obj["id"], 7);
        assert_eq!(obj["error"]["code"], code::FORBIDDEN_SCOPE);
        assert_eq!(obj["error"]["data"]["agentCode"], "FORBIDDEN_SCOPE");
        assert_eq!(obj["error"]["data"]["suggestion"], "use an admin token");
        assert_eq!(obj["error"]["data"]["httpStatus"], 403);
    }

    #[test]
    fn only_authorization_failures_are_protocol_failures() {
        assert!(is_protocol_failure(&AgentError::new(ErrorCode::Unauthorized, "m", "s")));
        assert!(is_protocol_failure(&AgentError::new(ErrorCode::ForbiddenScope, "m", "s")));
        assert!(is_protocol_failure(&AgentError::new(ErrorCode::RateLimited, "m", "s")));
        assert!(!is_protocol_failure(&AgentError::new(ErrorCode::InvalidUrl, "m", "s")));
        assert!(!is_protocol_failure(&AgentError::new(ErrorCode::Internal, "m", "s")));
    }

    #[test]
    fn protocol_version_is_negotiated() {
        assert_eq!(negotiate_version(Some("2024-11-05")), "2024-11-05");
        assert_eq!(negotiate_version(Some("2025-06-18")), "2025-06-18");
        // An unknown revision falls back to our newest rather than failing.
        assert_eq!(negotiate_version(Some("1999-01-01")), SUPPORTED_PROTOCOL_VERSIONS[0]);
        assert_eq!(negotiate_version(None), SUPPORTED_PROTOCOL_VERSIONS[0]);
    }

    #[test]
    fn advertised_capabilities_match_the_dispatch_table() {
        let caps = capabilities();
        assert!(caps["tools"].is_object());
        assert!(caps["resources"].is_object());
        assert!(caps["prompts"].is_object());
        for m in ["tools/list", "resources/list", "prompts/list"] {
            assert!(METHODS.contains(&m), "{m} is advertised but not dispatched");
        }
    }

    #[test]
    fn tool_lists_project_the_single_registry() {
        let list = tools_list();
        let tools = list["tools"].as_array().expect("tools array");
        let registry = tools_schema_list();
        let rows = registry.as_array().unwrap();
        assert_eq!(tools.len(), rows.len());
        for (row, spec) in tools.iter().zip(rows) {
            assert_eq!(row["name"], spec["name"]);
            assert!(row["description"].as_str().is_some_and(|d| !d.is_empty()));
            assert_eq!(row["inputSchema"]["type"], "object");
            // readOnlyHint must be the exact inverse of the mutating flag.
            let mutating = spec["mutating"].as_bool().unwrap_or(false);
            assert_eq!(row["annotations"]["readOnlyHint"], !mutating);
            assert_eq!(row["annotations"]["destructiveHint"], mutating);
        }
    }

    #[test]
    fn resource_lists_split_static_from_templated_without_loss() {
        let static_ids: Vec<String> = resources_list()["resources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["uri"].as_str().unwrap().to_string())
            .collect();
        let template_ids: Vec<String> = resource_templates_list()["resourceTemplates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["uriTemplate"].as_str().unwrap().to_string())
            .collect();
        for id in &static_ids {
            assert!(!id.contains('{'), "static list must not contain a template: {id}");
        }
        for id in &template_ids {
            assert!(id.contains('{'), "template list must only contain templates: {id}");
        }
        // No resource may be dropped or duplicated across the two lists.
        assert_eq!(static_ids.len() + template_ids.len(), RESOURCES.len());
    }

    #[test]
    fn prompts_list_projects_every_prompt() {
        let list = prompts_list();
        assert_eq!(list["prompts"].as_array().unwrap().len(), PROMPTS.len());
        for row in list["prompts"].as_array().unwrap() {
            assert!(row["name"].as_str().is_some_and(|n| !n.is_empty()));
            assert!(row["description"].as_str().is_some_and(|d| !d.is_empty()));
        }
    }

    #[test]
    fn tool_call_arguments_strips_dry_run_and_rejects_non_objects() {
        let mut map = serde_json::Map::new();
        map.insert("arguments".into(), json!({ "id": "x", "dryRun": true }));
        let (args, dry) = tool_call_arguments(&map).unwrap();
        assert!(dry);
        assert!(args.get("dryRun").is_none(), "dryRun must never reach the tool");
        assert_eq!(args["id"], "x");

        let mut bad = serde_json::Map::new();
        bad.insert("arguments".into(), json!("not an object"));
        assert!(tool_call_arguments(&bad).is_err());

        // Absent arguments means an empty object, not an error.
        let empty = serde_json::Map::new();
        let (args, dry) = tool_call_arguments(&empty).unwrap();
        assert_eq!(args, json!({}));
        assert!(!dry);
    }

    #[test]
    fn tool_result_and_tool_error_result_have_the_mcp_shape() {
        let ok = tool_result(json!({ "services": [] }));
        assert_eq!(ok["content"][0]["type"], "text");
        assert_eq!(ok["isError"], false);
        assert_eq!(ok["structuredContent"]["services"], json!([]));

        let err = AgentError::new(ErrorCode::InvalidUrl, "bad url", "use https");
        let bad = tool_error_result(&err);
        assert_eq!(bad["isError"], true);
        assert_eq!(bad["structuredContent"]["code"], "INVALID_URL");
        assert_eq!(bad["structuredContent"]["suggestion"], "use https");
    }

    #[test]
    fn prompt_result_wraps_a_single_user_message() {
        let r = prompt_result("add-messenger", "Add a messenger", "# Steps".into());
        assert_eq!(r["messages"][0]["role"], "user");
        assert_eq!(r["messages"][0]["content"]["type"], "text");
        assert_eq!(r["messages"][0]["content"]["text"], "# Steps");
    }

    #[test]
    fn envelope_validation_rejects_malformed_requests() {
        // Not an object at all.
        let err = read_envelope(&json!("nope")).unwrap_err();
        assert_eq!(err["error"]["code"], code::INVALID_REQUEST);

        // Wrong jsonrpc version: the id must still be echoed.
        let err = read_envelope(&json!({ "jsonrpc": "1.0", "id": 3, "method": "ping" })).unwrap_err();
        assert_eq!(err["error"]["code"], code::INVALID_REQUEST);
        assert_eq!(err["id"], 3);

        // Missing method.
        let err = read_envelope(&json!({ "jsonrpc": "2.0", "id": 4 })).unwrap_err();
        assert_eq!(err["error"]["code"], code::INVALID_REQUEST);

        // A well-formed envelope passes through.
        let (id, method, params) =
            read_envelope(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).unwrap();
        assert_eq!(id, 1);
        assert_eq!(method, "ping");
        assert!(params.is_none());
    }

    #[test]
    fn required_str_rejects_empty_and_missing_values() {
        let mut map = serde_json::Map::new();
        assert!(required_str(&map, "name", "tools/call").is_err());
        map.insert("name".into(), json!("   "));
        assert!(required_str(&map, "name", "tools/call").is_err(), "blank must be rejected");
        map.insert("name".into(), json!("list_services"));
        assert_eq!(required_str(&map, "name", "tools/call").unwrap(), "list_services");
        // A non-string must not be coerced.
        map.insert("name".into(), json!(42));
        assert!(required_str(&map, "name", "tools/call").is_err());
    }

    /// tools/call enforces scope through require_scope before the handler, so
    /// this asserts the property at the registry level and stays true as tools
    /// change.
    #[test]
    fn every_tool_above_read_scope_is_refused_to_a_read_token() {
        let mut checked = 0;
        for spec in all_tools() {
            if spec.scope == Scope::Read {
                assert!(require_scope(Scope::Read, spec.scope, spec.name).is_ok());
                continue;
            }
            let err = require_scope(Scope::Read, spec.scope, spec.name).unwrap_err();
            assert_eq!(err.code, "FORBIDDEN_SCOPE", "tool {} must refuse a read token", spec.name);
            checked += 1;
        }
        assert!(checked > 0, "the registry declares no tool above read scope");
    }

    /// A write token satisfies read and write tools but never admin tools.
    #[test]
    fn write_scope_is_sufficient_for_write_but_not_admin() {
        for spec in all_tools() {
            match spec.scope {
                Scope::Read | Scope::Write => {
                    assert!(require_scope(Scope::Write, spec.scope, spec.name).is_ok())
                }
                Scope::Admin => {
                    let err = require_scope(Scope::Write, spec.scope, spec.name).unwrap_err();
                    assert_eq!(err.code, "FORBIDDEN_SCOPE");
                }
            }
        }
    }
}
