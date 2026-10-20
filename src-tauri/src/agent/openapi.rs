//! OpenAPI 3.1.0 projection of the agent registry.
//!
//! The document is GENERATED, never hand-maintained: every path, parameter,
//! request body and component schema is derived from the same ToolSpec,
//! ResourceSpec and PromptSpec tables that the MCP server and the REST routes
//! serve. If a tool is added, it appears here with no second edit -- which is
//! the only way a discovery document stays true.
//!
//! OpenAPI 3.1 is used (not 3.0) because 3.1 aligns with JSON Schema 2020-12,
//! so a tool's inputSchema is embedded verbatim instead of being translated.

use serde_json::{json, Map, Value};

use super::error::ErrorCode;
use super::prompts::PROMPTS;
use super::resources::RESOURCES;
use super::tools::all_tools;
use super::{PromptSpec, ResourceSpec, ToolSpec};

/// Every REST route lives under this prefix. Kept as a const so the server,
/// the manifest and this document cannot drift apart.
pub const API_PREFIX: &str = "/api/v1/agent";

/// The JSON-RPC endpoint that carries MCP over streamable HTTP.
pub const MCP_PATH: &str = "/mcp";

/// Prefixes a route fragment with the API prefix.
pub fn route(fragment: &str) -> String {
    if fragment == "/" {
        format!("{API_PREFIX}/")
    } else {
        format!("{API_PREFIX}{fragment}")
    }
}

fn bearer_security() -> Value {
    json!([{ "bearerAuth": [] }])
}

fn ok_json(description: &str, schema: Value) -> Value {
    json!({
        "description": description,
        "content": { "application/json": { "schema": schema } }
    })
}

fn err_response(description: &str) -> Value {
    json!({
        "description": description,
        "content": {
            "application/json": { "schema": { "$ref": "#/components/schemas/AgentError" } }
        }
    })
}

/// The standard failure set every route can produce. Written once and reused
/// so the document never claims a route can fail in ways the server cannot.
fn common_errors() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("400".into(), err_response("Invalid parameters. Body is an AgentError with code INVALID_PARAMETER."));
    m.insert("401".into(), err_response("Missing or invalid bearer token. Code UNAUTHORIZED."));
    m.insert("403".into(), err_response("Refused before or after authentication: a browser Origin header was present, the Host header was not loopback, or the token scope is insufficient. Codes FORBIDDEN_SCOPE."));
    m.insert("405".into(), err_response("Wrong method, or a CORS preflight attempt. Code METHOD_NOT_ALLOWED."));
    m.insert("429".into(), err_response("Per-token rate limit exceeded. Code RATE_LIMITED. Honour the Retry-After header."));
    m.insert("500".into(), err_response("Unexpected internal failure. Code INTERNAL_ERROR."));
    m
}

/// The 200 response describing a tool result.
fn tool_result_response(spec_name: &str) -> Value {
    ok_json(
        &format!("Result of {spec_name}. Dry runs return {{ dryRun, tool, applied, wouldApply, note }}."),
        json!({ "type": "object", "description": "Tool result object.", "additionalProperties": true }),
    )
}

/// One documented tool call. Used for both the concrete per-tool path and the
/// generic name-driven path, so the two can never disagree.
fn tool_call_operation(spec: Option<&ToolSpec>) -> Value {
    let (name_param, summary, description, request_schema, extra_param) = match spec {
        Some(s) => (
            json!({
                "name": "name",
                "in": "path",
                "required": true,
                "description": format!("Path constant for this operation. Always the literal '{}'.", s.name),
                "schema": { "type": "string", "const": s.name }
            }),
            s.description.to_string(),
            format!(
                "{} Required token scope: {}. Mutating: {}.{}",
                s.description,
                s.scope,
                s.mutating,
                if s.mutating && s.preview.is_some() {
                    " Supplying dryRun: true validates the request and returns the projected effect without committing anything."
                } else {
                    ""
                }
            ),
            json!({ "$ref": format!("#/components/schemas/ToolInput_{}", s.name) }),
            s.mutating,
        ),
        None => (
            json!({
                "name": "name",
                "in": "path",
                "required": true,
                "description": "The tool name. Call GET /tools for the registered names.",
                "schema": { "type": "string" }
            }),
            "Execute any registered tool by name.".to_string(),
            "One generic route for every tool. Each tool also has its own documented path, which is the preferred form because the input schema is pinned.".to_string(),
            json!({ "type": "object", "description": "Arguments object. The per-tool shape is that tool's inputSchema.", "additionalProperties": true }),
            true,
        ),
    };

    let mut params = vec![name_param];
    if extra_param {
        params.push(json!({
            "name": "dryRun",
            "in": "query",
            "required": false,
            "description": "Set to true to validate the request and return the projected effect without committing it. Ignored by read-only tools.",
            "schema": { "type": "boolean", "default": false }
        }));
    }

    let mut responses = common_errors();
    responses.insert("200".into(), tool_result_response(spec.map_or("the requested tool", |s| s.name)));
    responses.insert("404".into(), err_response("Unknown tool name. Code UNKNOWN_TOOL."));
    responses.insert(
        "403".into(),
        err_response("Browser-origin request, non-loopback Host, or a token scope below what the tool requires. Codes FORBIDDEN_SCOPE."),
    );

    json!({
        "operationId": spec.map_or_else(|| "execute_tool".to_string(), |s| format!("tool_{}", s.name)),
        "summary": summary,
        "description": description,
        "tags": ["tools"],
        "security": bearer_security(),
        "parameters": params,
        "requestBody": {
            "required": false,
            "description": "Arguments for the tool. Unknown properties are rejected by the tool schema.",
            "content": { "application/json": { "schema": request_schema } }
        },
        "responses": Value::Object(responses)
    })
}

fn resource_operation(spec: &ResourceSpec) -> Value {
    let rest = resource_path_template(spec.uri_template);
    let mut params = vec![json!({
        "name": "rest",
        "in": "path",
        "required": true,
        "description": format!("Path after 'bzdium://'. For '{}' supply '{}'.", spec.uri_template, rest.trim_start_matches('/')),
        "schema": { "type": "string" }
    })];
    for name in template_params(spec.uri_template) {
        params.push(json!({
            "name": name,
            "in": "path",
            "required": true,
            "description": format!("The '{name}' segment of {}.", spec.uri_template),
            "schema": { "type": "string" }
        }));
    }
    let mut responses = common_errors();
    responses.insert(
        "200".into(),
        json!({
            "description": format!("The resource rendered as {}.", spec.mime_type),
            "content": { spec.mime_type: { "schema": { "type": "string" } } }
        }),
    );
    responses.insert("404".into(), err_response("Unknown resource URI. Code UNKNOWN_RESOURCE."));
    json!({
        "operationId": format!("resource_{}", operation_id(spec.uri_template)),
        "summary": spec.description,
        "description": format!(
            "Reads the {} resource ({}). Required token scope: {}. Service session directories expose names and sizes only, never profile contents.",
            spec.name, spec.uri_template, spec.scope
        ),
        "tags": ["resources"],
        "security": bearer_security(),
        "parameters": params,
        "responses": Value::Object(responses)
    })
}

fn prompt_operation(spec: &PromptSpec) -> Value {
    let args: Vec<Value> = (spec.arguments)()
        .into_iter()
        .map(|a| {
            json!({
                "name": a.name,
                "in": "query",
                "required": a.required,
                "description": a.description,
                "schema": { "type": "string" }
            })
        })
        .collect();
    let mut responses = common_errors();
    responses.insert(
        "200".into(),
        json!({
            "description": "The rendered prompt.",
            "content": {
                "text/markdown": { "schema": { "type": "string" } },
                "application/json": { "schema": { "$ref": "#/components/schemas/RenderedPrompt" } }
            }
        }),
    );
    responses.insert("404".into(), err_response("Unknown prompt name. Code UNKNOWN_PROMPT."));
    json!({
        "operationId": format!("prompt_{}", operation_id(spec.name)),
        "summary": spec.description,
        "description": format!(
            "Renders the '{}' workflow template as markdown. Required token scope: {}.",
            spec.name, spec.scope
        ),
        "tags": ["prompts"],
        "security": bearer_security(),
        "parameters": args,
        "responses": Value::Object(responses)
    })
}

/// Turns a bzdium:// URI template into a REST path fragment.
/// bzdium://services/{id} -> /resources/services/{id}
pub fn resource_path_template(uri_template: &str) -> String {
    let rest = uri_template
        .strip_prefix("bzdium://")
        .unwrap_or(uri_template)
        .trim_start_matches('/');
    format!("/resources/{rest}")
}

/// Collects the braced segment names of a URI template, in order.
pub fn template_params(uri_template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = uri_template;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else { break };
        let name = &after[..end];
        if !name.is_empty() {
            out.push(name.to_string());
        }
        rest = &after[end + 1..];
    }
    out
}

/// A stable, ASCII-safe identifier for operationIds and schema names.
pub fn operation_id(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

fn error_codes() -> Vec<&'static str> {
    [
        ErrorCode::Unauthorized,
        ErrorCode::ForbiddenScope,
        ErrorCode::RateLimited,
        ErrorCode::NotFound,
        ErrorCode::MethodNotAllowed,
        ErrorCode::InvalidParameter,
        ErrorCode::UnknownTool,
        ErrorCode::UnknownResource,
        ErrorCode::UnknownPrompt,
        ErrorCode::UnknownServiceId,
        ErrorCode::InvalidUrl,
        ErrorCode::TlsBypassRefused,
        ErrorCode::ServiceDisabled,
        ErrorCode::InvalidNavAction,
        ErrorCode::ConfigInvalid,
        ErrorCode::ConfigTooLarge,
        ErrorCode::ConfirmationRequired,
        ErrorCode::Internal,
        ErrorCode::NotRunning,
    ]
    .into_iter()
    .map(|c| c.as_str())
    .collect()
}

fn component_schemas() -> Map<String, Value> {
    let mut schemas = Map::new();

    schemas.insert("AgentError".into(), json!({
        "type": "object",
        "description": "The single failure envelope every route returns on a non-2xx status. The suggestion field exists so a calling model can correct its own request without guessing.",
        "required": ["code", "message", "suggestion"],
        "properties": {
            "code": { "type": "string", "description": "Stable machine-readable failure code. Branch on this, never on the message text.", "enum": error_codes() },
            "message": { "type": "string", "description": "Human-readable description of what went wrong." },
            "suggestion": { "type": "string", "description": "Corrective instruction the caller can act on directly." },
            "details": { "type": "object", "description": "Optional structured context, such as the rejected field or the offending value.", "additionalProperties": true }
        },
        "additionalProperties": false
    }));

    schemas.insert("Health".into(), json!({
        "type": "object",
        "description": "Liveness and identity of the loopback listener.",
        "required": ["status", "version", "uptimeSecs", "transport", "agentEnabled"],
        "properties": {
            "status": { "type": "string", "description": "The literal 'ok' while the server can answer.", "enum": ["ok"] },
            "version": { "type": "string", "description": "The bzdium release version currently running." },
            "uptimeSecs": { "type": "integer", "minimum": 0, "description": "Whole seconds since the agent context was created." },
            "transport": { "type": "string", "description": "Transports served on this port, e.g. 'rest, mcp-http'." },
            "agentEnabled": { "type": "boolean", "description": "True when the master agent switch is on. This endpoint is only answered while it is." }
        },
        "additionalProperties": false
    }));

    schemas.insert("ToolSummary".into(), json!({
        "type": "object",
        "description": "One registered tool, as listed by GET /tools and MCP tools/list.",
        "required": ["name", "description", "scope", "mutating", "inputSchema"],
        "properties": {
            "name": { "type": "string", "description": "Tool name, identical in REST and MCP." },
            "description": { "type": "string", "description": "What the tool does, written for a model caller." },
            "scope": { "type": "string", "description": "Minimum token scope required.", "enum": ["read", "write", "admin"] },
            "mutating": { "type": "boolean", "description": "True when the tool can change persisted or runtime state." },
            "supportsDryRun": { "type": "boolean", "description": "True when the tool can be previewed with dryRun." },
            "inputSchema": { "type": "object", "description": "JSON Schema (2020-12 subset) for this tool's arguments." }
        },
        "additionalProperties": false
    }));

    schemas.insert("ResourceSummary".into(), json!({
        "type": "object",
        "description": "One registered resource, as listed by GET /resources.",
        "required": ["uri", "name", "description", "mimeType", "scope"],
        "properties": {
            "uri": { "type": "string", "description": "The resource URI template, e.g. bzdium://services/{id}." },
            "name": { "type": "string", "description": "Short human label." },
            "description": { "type": "string", "description": "What the resource returns." },
            "mimeType": { "type": "string", "description": "Media type of the rendered body." },
            "scope": { "type": "string", "description": "Minimum token scope required to read it.", "enum": ["read", "write", "admin"] },
            "restPath": { "type": "string", "description": "The REST path that reads this resource." }
        },
        "additionalProperties": false
    }));

    schemas.insert("PromptSummary".into(), json!({
        "type": "object",
        "description": "One registered prompt template, as listed by GET /prompts.",
        "required": ["name", "description", "scope", "arguments"],
        "properties": {
            "name": { "type": "string", "description": "Prompt name used with GET /prompts/{name} and MCP prompts/get." },
            "description": { "type": "string", "description": "What workflow the template drives." },
            "scope": { "type": "string", "description": "Minimum token scope required.", "enum": ["read", "write", "admin"] },
            "arguments": {
                "type": "array",
                "description": "Arguments the template accepts.",
                "items": {
                    "type": "object",
                    "required": ["name", "description", "required"],
                    "properties": {
                        "name": { "type": "string", "description": "Argument name." },
                        "description": { "type": "string", "description": "What the argument means." },
                        "required": { "type": "boolean", "description": "True when the template cannot render without it." }
                    },
                    "additionalProperties": false
                }
            }
        },
        "additionalProperties": false
    }));

    schemas.insert("RenderedPrompt".into(), json!({
        "type": "object",
        "description": "A rendered prompt template.",
        "required": ["name", "description", "text"],
        "properties": {
            "name": { "type": "string", "description": "Prompt name." },
            "description": { "type": "string", "description": "Prompt description." },
            "text": { "type": "string", "description": "The rendered markdown instruction template." }
        },
        "additionalProperties": false
    }));

    schemas.insert("Manifest".into(), json!({
        "type": "object",
        "description": "Discovery document: what this agent surface can do, in one request.",
        "required": ["name", "version", "apiVersion", "basePath", "counts", "tools", "resources", "prompts"],
        "properties": {
            "name": { "type": "string", "description": "Product name." },
            "version": { "type": "string", "description": "bzdium release version." },
            "apiVersion": { "type": "string", "description": "Agent API surface version." },
            "basePath": { "type": "string", "description": "Prefix every REST route is mounted under." },
            "counts": {
                "type": "object",
                "description": "Registry sizes, so a client can detect a truncated list.",
                "required": ["tools", "resources", "prompts"],
                "properties": {
                    "tools": { "type": "integer", "description": "Registered tool count." },
                    "resources": { "type": "integer", "description": "Registered resource count." },
                    "prompts": { "type": "integer", "description": "Registered prompt count." }
                },
                "additionalProperties": false
            },
            "tools": { "type": "array", "description": "Every registered tool with its scope and mutability.", "items": { "$ref": "#/components/schemas/ToolSummary" } },
            "resources": { "type": "array", "description": "Every registered resource.", "items": { "$ref": "#/components/schemas/ResourceSummary" } },
            "prompts": { "type": "array", "description": "Every registered prompt.", "items": { "$ref": "#/components/schemas/PromptSummary" } }
        },
        "additionalProperties": false
    }));

    schemas.insert("JsonRpcRequest".into(), json!({
        "type": "object",
        "description": "A JSON-RPC 2.0 request. Notifications, which omit id, are accepted and produce no response object.",
        "required": ["jsonrpc", "method"],
        "properties": {
            "jsonrpc": { "type": "string", "description": "Protocol version. Must be the literal '2.0'.", "enum": ["2.0"] },
            "id": { "description": "Request identifier echoed in the response. Omit for a notification.", "type": ["string", "integer", "null"] },
            "method": { "type": "string", "description": "One of initialize, ping, tools/list, tools/call, resources/list, resources/read, prompts/list, prompts/get." },
            "params": { "type": "object", "description": "Method parameters.", "additionalProperties": true }
        },
        "additionalProperties": false
    }));

    schemas.insert("JsonRpcResponse".into(), json!({
        "type": "object",
        "description": "A JSON-RPC 2.0 response. Exactly one of result or error is present.",
        "required": ["jsonrpc"],
        "properties": {
            "jsonrpc": { "type": "string", "description": "Always the literal '2.0'.", "enum": ["2.0"] },
            "id": { "description": "Echo of the request id, or null when the id could not be read.", "type": ["string", "integer", "null"] },
            "result": { "type": "object", "description": "Method result on success.", "additionalProperties": true },
            "error": {
                "type": "object",
                "description": "Failure, carrying the agent error chrome under data.",
                "required": ["code", "message"],
                "properties": {
                    "code": { "type": "integer", "description": "JSON-RPC error code." },
                    "message": { "type": "string", "description": "Short failure description." },
                    "data": { "type": "object", "description": "Agent error details: agentCode, suggestion, details.", "additionalProperties": true }
                },
                "additionalProperties": false
            }
        },
        "additionalProperties": false
    }));

    // Each tool's inputSchema is embedded verbatim under its own component so
    // the request body can reference it and a 3.1 validator resolves it directly.
    for spec in all_tools() {
        schemas.insert(format!("ToolInput_{}", spec.name), (spec.input_schema)());
    }

    schemas
}

fn tags() -> Value {
    json!([
        { "name": "meta", "description": "Health, discovery and the generated OpenAPI document." },
        { "name": "tools", "description": "Tool invocation. Every mutating tool accepts dryRun." },
        { "name": "resources", "description": "Read-only context documents addressed by bzdium:// URIs." },
        { "name": "prompts", "description": "Multi-step workflow templates rendered as markdown." },
        { "name": "mcp", "description": "Model Context Protocol over streamable HTTP (JSON-RPC 2.0)." }
    ])
}

/// Builds the complete OpenAPI 3.1.0 document from the live registry.
pub fn document() -> Value {
    let mut paths = Map::new();

    paths.insert(route("/health"), json!({ "get": {
        "operationId": "get_health",
        "summary": "Liveness and identity of the loopback agent listener.",
        "tags": ["meta"],
        "security": bearer_security(),
        "responses": {
            "200": ok_json("Current health.", json!({ "$ref": "#/components/schemas/Health" })),
            "401": err_response("Missing or invalid bearer token. Code UNAUTHORIZED."),
            "403": err_response("Refused by the request-hygiene gate: Origin header present, Host not loopback, or peer not local. Code FORBIDDEN_SCOPE."),
            "405": err_response("Wrong method, or a CORS preflight attempt. Code METHOD_NOT_ALLOWED."),
            "429": err_response("Rate limit exceeded. Code RATE_LIMITED."),
            "500": err_response("Unexpected internal failure. Code INTERNAL_ERROR.")
        }
    }}));

    paths.insert(route("/manifest"), json!({ "get": {
        "operationId": "get_manifest",
        "summary": "Discovery document listing every tool, resource and prompt with its scope.",
        "description": "Start here. One request tells an agent everything this surface can do, so it never has to guess a tool name.",
        "tags": ["meta"],
        "security": bearer_security(),
        "responses": {
            "200": ok_json("The manifest.", json!({ "$ref": "#/components/schemas/Manifest" })),
            "401": err_response("Missing or invalid bearer token. Code UNAUTHORIZED."),
            "403": err_response("Refused by the request-hygiene gate: Origin header present, Host not loopback, or peer not local. Code FORBIDDEN_SCOPE."),
            "405": err_response("Wrong method, or a CORS preflight attempt. Code METHOD_NOT_ALLOWED."),
            "429": err_response("Rate limit exceeded. Code RATE_LIMITED."),
            "500": err_response("Unexpected internal failure. Code INTERNAL_ERROR.")
        }
    }}));

    paths.insert(route("/openapi.json"), json!({ "get": {
        "operationId": "get_openapi",
        "summary": "This document.",
        "description": "The OpenAPI 3.1 document generated from the live tool, resource and prompt registries.",
        "tags": ["meta"],
        "security": bearer_security(),
        "responses": {
            "200": ok_json("The OpenAPI document.", json!({ "type": "object", "description": "An OpenAPI 3.1.0 document.", "additionalProperties": true })),
            "401": err_response("Missing or invalid bearer token. Code UNAUTHORIZED."),
            "403": err_response("Refused by the request-hygiene gate: Origin header present, Host not loopback, or peer not local. Code FORBIDDEN_SCOPE."),
            "429": err_response("Rate limit exceeded. Code RATE_LIMITED.")
        }
    }}));

    let mut list_tools_responses = common_errors();
    list_tools_responses.insert("200".into(), ok_json("The tool list.", json!({
        "type": "object",
        "required": ["tools", "count"],
        "properties": {
            "tools": { "type": "array", "description": "Registered tools.", "items": { "$ref": "#/components/schemas/ToolSummary" } },
            "count": { "type": "integer", "description": "Number of tools returned." }
        },
        "additionalProperties": false
    })));
    paths.insert(route("/tools"), json!({ "get": {
        "operationId": "list_tools",
        "summary": "Every registered tool with its input schema and required scope.",
        "tags": ["tools"],
        "security": bearer_security(),
        "responses": Value::Object(list_tools_responses)
    }}));

    // The generic name-driven route the server actually dispatches on.
    paths.insert(route("/tools/{name}"), json!({ "post": tool_call_operation(None) }));

    // One concrete path per tool, so every registered tool is discoverable as
    // a path with its input schema pinned. Concrete paths take precedence over
    // the templated one, and both reach the same handler.
    for spec in all_tools() {
        paths.insert(route(&format!("/tools/{}", spec.name)), json!({ "post": tool_call_operation(Some(spec)) }));
    }

    let mut list_resources_responses = common_errors();
    list_resources_responses.insert("200".into(), ok_json("The resource list.", json!({
        "type": "object",
        "required": ["resources", "count"],
        "properties": {
            "resources": { "type": "array", "description": "Registered resources.", "items": { "$ref": "#/components/schemas/ResourceSummary" } },
            "count": { "type": "integer", "description": "Number of resources returned." }
        },
        "additionalProperties": false
    })));
    paths.insert(route("/resources"), json!({ "get": {
        "operationId": "list_resources",
        "summary": "Every registered resource URI template.",
        "tags": ["resources"],
        "security": bearer_security(),
        "responses": Value::Object(list_resources_responses)
    }}));

    for spec in RESOURCES {
        paths.insert(route(&resource_path_template(spec.uri_template)), json!({ "get": resource_operation(spec) }));
    }

    let mut list_prompts_responses = common_errors();
    list_prompts_responses.insert("200".into(), ok_json("The prompt list.", json!({
        "type": "object",
        "required": ["prompts", "count"],
        "properties": {
            "prompts": { "type": "array", "description": "Registered prompts.", "items": { "$ref": "#/components/schemas/PromptSummary" } },
            "count": { "type": "integer", "description": "Number of prompts returned." }
        },
        "additionalProperties": false
    })));
    paths.insert(route("/prompts"), json!({ "get": {
        "operationId": "list_prompts",
        "summary": "Every registered prompt template.",
        "tags": ["prompts"],
        "security": bearer_security(),
        "responses": Value::Object(list_prompts_responses)
    }}));

    for spec in PROMPTS {
        paths.insert(route(&format!("/prompts/{}", spec.name)), json!({ "get": prompt_operation(spec) }));
    }

    let mut mcp_responses = common_errors();
    mcp_responses.insert("200".into(), ok_json("The JSON-RPC response, or an array of responses for a batch.", json!({ "$ref": "#/components/schemas/JsonRpcResponse" })));
    paths.insert(route(MCP_PATH), json!({ "post": {
        "operationId": "mcp_json_rpc",
        "summary": "Model Context Protocol over streamable HTTP.",
        "description": "Accepts one JSON-RPC 2.0 request object, or a batch array, and returns the response. Supported methods: initialize, ping, tools/list, tools/call, resources/list, resources/read, prompts/list, prompts/get. Same authentication, Origin rejection, Host validation, scope enforcement and rate limiting as the REST routes.",
        "tags": ["mcp"],
        "security": bearer_security(),
        "requestBody": {
            "required": true,
            "description": "A JSON-RPC 2.0 request object, or an array of them for a batch.",
            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/JsonRpcRequest" } } }
        },
        "responses": Value::Object(mcp_responses)
    }}));

    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "bzdium Agent Access API",
            "version": env!("CARGO_PKG_VERSION"),
            "summary": "Local, token-gated control of a running bzdium desktop app.",
            "description": "Every operation here delegates to the same Rust function the desktop UI calls over Tauri IPC. The listener binds 127.0.0.1 only, rejects any request carrying an Origin header, validates the Host header against DNS rebinding, requires a bearer token compared in constant time, and enforces a per-token rate limit. CORS preflight is never answered.",
            "license": { "name": "MIT" }
        },
        "servers": [{
            "url": "http://127.0.0.1:{port}",
            "description": "Loopback listener. Read the actual port from <dataDir>/agent/port.",
            "variables": { "port": { "default": "47821", "description": "The ephemeral port recorded by the running bzdium instance." } }
        }],
        "tags": tags(),
        "paths": Value::Object(paths),
        "components": {
            "securitySchemes": {
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer",
                    "format": "bzd_agt_<opaque>",
                    "description": "Token minted inside bzdium. Its scope, read < write < admin, bounds which operations it may call."
                }
            },
            "schemas": Value::Object(component_schemas())
        }
    })
}

/// Convenience for the /openapi.json route: the document as pretty JSON.
pub fn document_json() -> String {
    match serde_json::to_string_pretty(&document()) {
        Ok(text) => text,
        Err(e) => {
            // A serialisation failure would be a programming error, not user
            // input; still emit valid JSON rather than a broken document.
            json!({
                "openapi": "3.1.0",
                "info": { "title": "bzdium Agent Access API", "version": env!("CARGO_PKG_VERSION") },
                "error": {
                    "code": "INTERNAL_ERROR",
                    "message": format!("cannot serialise the OpenAPI document: {e}"),
                    "suggestion": "This is a bzdium bug. Report it with the version shown in GET /health."
                }
            })
            .to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_declares_openapi_3_1_0() {
        assert_eq!(document()["openapi"], "3.1.0");
    }

    #[test]
    fn every_registered_tool_appears_as_a_path() {
        let doc = document();
        let paths = doc["paths"].as_object().expect("paths object");
        for spec in all_tools() {
            let key = route(&format!("/tools/{}", spec.name));
            assert!(paths.contains_key(&key), "tool '{}' is missing its path {key}", spec.name);
            let op = &paths[&key]["post"];
            assert_eq!(op["operationId"], format!("tool_{}", spec.name));
            let body_ref = op["requestBody"]["content"]["application/json"]["schema"]["$ref"]
                .as_str()
                .unwrap_or_default();
            assert_eq!(body_ref, format!("#/components/schemas/ToolInput_{}", spec.name));
        }
    }

    #[test]
    fn core_routes_are_documented() {
        let doc = document();
        let paths = doc["paths"].as_object().unwrap();
        for fragment in [
            "/health", "/manifest", "/openapi.json", "/tools", "/tools/{name}",
            "/resources", "/prompts", MCP_PATH,
        ] {
            assert!(paths.contains_key(&route(fragment)), "missing route {fragment}");
        }
    }

    #[test]
    fn every_resource_and_prompt_has_a_path() {
        let doc = document();
        let paths = doc["paths"].as_object().unwrap();
        for r in RESOURCES {
            let p = route(&resource_path_template(r.uri_template));
            assert!(paths.contains_key(&p), "resource {} missing path {p}", r.uri_template);
        }
        for p in PROMPTS {
            let key = route(&format!("/prompts/{}", p.name));
            assert!(paths.contains_key(&key), "prompt {} missing path {key}", p.name);
        }
    }

    #[test]
    fn bearer_security_scheme_is_declared_and_applied() {
        let doc = document();
        let scheme = &doc["components"]["securitySchemes"]["bearerAuth"];
        assert_eq!(scheme["type"], "http");
        assert_eq!(scheme["scheme"], "bearer");
        let mcp = &doc["paths"][route(MCP_PATH)]["post"];
        assert_eq!(mcp["security"][0]["bearerAuth"], json!([]));
        let tools = &doc["paths"][route("/tools")]["get"];
        assert_eq!(tools["security"][0]["bearerAuth"], json!([]));
    }

    #[test]
    fn every_tool_schema_is_embedded_as_a_component() {
        let doc = document();
        let schemas = doc["components"]["schemas"].as_object().unwrap();
        for spec in all_tools() {
            let name = format!("ToolInput_{}", spec.name);
            assert!(schemas.contains_key(&name), "missing component schema {name}");
            assert_eq!(schemas[&name]["type"], "object");
            assert!(schemas[&name]["properties"].is_object());
        }
    }

    #[test]
    fn error_codes_are_all_documented() {
        let doc = document();
        let codes = doc["components"]["schemas"]["AgentError"]["properties"]["code"]["enum"]
            .as_array()
            .unwrap()
            .clone();
        for c in error_codes() {
            assert!(codes.iter().any(|v| v == c), "error code {c} missing from the enum");
        }
    }

    #[test]
    fn every_operation_has_a_unique_operation_id() {
        let doc = document();
        let mut seen = std::collections::HashSet::new();
        for (path_key, item) in doc["paths"].as_object().unwrap() {
            for method in ["get", "post", "put", "delete"] {
                if let Some(op) = item.get(method) {
                    let id = op["operationId"].as_str().unwrap_or_default().to_string();
                    assert!(!id.is_empty(), "{path_key} {method} has no operationId");
                    assert!(seen.insert(id.clone()), "duplicate operationId {id}");
                }
            }
        }
    }

    #[test]
    fn resource_path_template_maps_uri_to_rest() {
        assert_eq!(resource_path_template("bzdium://services/{id}"), "/resources/services/{id}");
        assert_eq!(resource_path_template("bzdium://app/state"), "/resources/app/state");
    }

    #[test]
    fn template_params_are_extracted_in_order() {
        assert_eq!(template_params("bzdium://services/{id}"), vec!["id".to_string()]);
        assert_eq!(template_params("bzdium://a/{x}/b/{y}"), vec!["x".to_string(), "y".to_string()]);
        assert!(template_params("bzdium://app/state").is_empty());
        assert!(template_params("bzdium://a/{unclosed").is_empty());
    }

    #[test]
    fn operation_id_is_ascii_safe() {
        assert_eq!(operation_id("bzdium://services/{id}"), "bzdium_services_id");
        assert_eq!(operation_id("/prompts/add-messenger"), "prompts_add_messenger");
    }

    #[test]
    fn document_serialises_to_valid_json() {
        let text = document_json();
        let parsed: Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(parsed["openapi"], "3.1.0");
        assert!(parsed["info"]["title"].as_str().unwrap().contains("bzdium"));
    }
}
