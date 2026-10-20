//! Cross-transport contract tests for the Agent Access Layer.
//!
//! These assert the invariants that must hold ACROSS MCP, REST and OpenAPI.
//! A unit test inside one module cannot catch a disagreement between two
//! surfaces; that is exactly the drift this file exists to prevent.
//!
//! Everything here is pure: no Tauri AppHandle, no socket, no filesystem.

use serde_json::Value;

use bzdium_lib::agent::{http, mcp, openapi, prompts, resources, tools};

fn tool_names() -> Vec<&'static str> {
    tools::all_tools().iter().map(|t| t.name).collect()
}

// ---------------------------------------------------------------------------
// 1. One registry, three surfaces
// ---------------------------------------------------------------------------

#[test]
fn mcp_rest_and_openapi_advertise_the_same_tool_set() {
    let canonical = tool_names();
    assert!(!canonical.is_empty(), "the registry must not be empty");

    // MCP tools/list
    let mcp_list = mcp::tools_list();
    let mcp_names: Vec<&str> = mcp_list["tools"]
        .as_array()
        .expect("MCP tools/list must have a tools array")
        .iter()
        .map(|t| t["name"].as_str().expect("every tool needs a name"))
        .collect();

    // REST GET /tools (same shape, so this also pins the REST projection).
    // Bind the Value first: it is a temporary we borrow from.
    let schema_list = tools::tools_schema_list();
    let rest_names: Vec<&str> = schema_list
        .as_array()
        .expect("the schema list must be an array")
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();

    assert_eq!(mcp_list["tools"].as_array().unwrap().len(), canonical.len());
    assert_eq!(rest_names.len(), canonical.len());
    assert_eq!(mcp_names, canonical, "MCP drifted from the registry");
    assert_eq!(rest_names, canonical, "REST drifted from the registry");
}

#[test]
fn every_openapi_operation_names_a_real_tool() {
    let doc = openapi::document();
    let paths = doc["paths"].as_object().expect("paths must be an object");
    let known = tool_names();

    let mut tool_paths_seen = 0usize;
    for (path, item) in paths {
        let Some(rest) = path.strip_prefix("/api/v1/agent/tools/") else {
            continue;
        };
        // The document also publishes the generic templated form
        // /tools/{name}; it is a template, not a concrete tool.
        if rest.starts_with('{') {
            continue;
        }
        tool_paths_seen += 1;
        assert!(
            known.contains(&rest),
            "OpenAPI documents /tools/{rest} but no such tool is registered"
        );
        assert!(
            item.get("post").is_some(),
            "tool path {path} must be documented as POST"
        );
    }
    assert_eq!(
        tool_paths_seen,
        known.len(),
        "every registered tool must appear in the OpenAPI document"
    );
}

#[test]
fn every_openapi_operation_id_is_unique() {
    let doc = openapi::document();
    let mut seen = std::collections::HashSet::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            if let Some(id) = op.get("operationId").and_then(Value::as_str) {
                assert!(
                    seen.insert(id.to_string()),
                    "duplicate operationId {id:?} at {method} {path}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 2. The routing contract
// ---------------------------------------------------------------------------

#[test]
fn every_registered_tool_has_a_reachable_rest_route() {
    for name in tool_names() {
        let path = format!("{}/tools/{}", openapi::API_PREFIX, name);
        match http::resolve_route(&path) {
            Some(http::Route::CallTool(found)) => assert_eq!(found, name),
            other => panic!("{path} resolved to {other:?}, expected CallTool({name})"),
        }
    }
}

#[test]
fn the_mcp_endpoint_resolves_to_its_own_route() {
    let path = format!("{}{}", openapi::API_PREFIX, openapi::MCP_PATH);
    assert_eq!(http::resolve_route(&path), Some(http::Route::Mcp));
}

#[test]
fn no_route_resolves_outside_the_api_prefix() {
    for path in [
        "/",
        "/health",
        "/api/v1/tools",
        "/api/v2/agent/tools",
        "/tools/list_services",
        "/mcp",
    ] {
        assert_eq!(
            http::resolve_route(path),
            None,
            "{path} must not resolve: the layer is namespaced under {}",
            openapi::API_PREFIX
        );
    }
}

#[test]
fn tool_routes_are_post_and_reads_are_get() {
    // A tool call mutates or computes; it is never a cacheable GET.
    let call = http::resolve_route(&format!("{}/tools/add_service", openapi::API_PREFIX));
    assert_eq!(call.map(|r| r.method().as_str().to_string()), Some("POST".into()));

    let health = http::resolve_route(&format!("{}/health", openapi::API_PREFIX));
    assert_eq!(health.map(|r| r.method().as_str().to_string()), Some("GET".into()));
}

// ---------------------------------------------------------------------------
// 3. Resources: MCP URIs and REST paths describe the same set
// ---------------------------------------------------------------------------

#[test]
fn every_resource_has_a_rest_path_and_a_mcp_uri() {
    let list = resources::resources_list();
    let rows = list.as_array().expect("resources_list must return an array");
    assert!(!rows.is_empty());

    // MCP splits resources by addressability, exactly as the spec requires:
    // concrete URIs go in resources/list, and parameterised ones go in
    // resourceTemplates/list. A template cannot be READ without its arguments,
    // so advertising it as a resource would be a lie. ResourceSpec only models
    // the template form, so a braced uri_template is a template and an
    // unbraced one is a concrete resource.
    let concrete = mcp::resources_list();
    let concrete: Vec<&str> = concrete["resources"]
        .as_array()
        .expect("resources/list must have a resources array")
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();

    let templates = mcp::resource_templates_list();
    let templates: Vec<&str> = templates["resourceTemplates"]
        .as_array()
        .expect("resourceTemplates/list must have a resourceTemplates array")
        .iter()
        .map(|r| r["uriTemplate"].as_str().unwrap())
        .collect();

    for row in rows {
        let uri = row["uri"].as_str().expect("every resource needs a uri");
        assert!(
            uri.starts_with("bzdium://"),
            "resource {uri} must use the bzdium:// scheme"
        );
        if uri.contains('{') {
            assert!(
                templates.contains(&uri),
                "template {uri} is missing from MCP resourceTemplates/list"
            );
            assert!(
                !concrete.contains(&uri),
                "template {uri} must not be advertised as a readable resource"
            );
        } else {
            assert!(
                concrete.contains(&uri),
                "resource {uri} is missing from MCP resources/list"
            );
        }
    }

    // The two lists together must cover the registry exactly.
    assert_eq!(
        concrete.len() + templates.len(),
        rows.len(),
        "MCP lists must cover every registered resource"
    );
}

#[test]
fn resource_templates_round_trip_through_the_http_layer() {
    // A template parameter must survive the URL form the REST layer uses.
    let uri = "bzdium://services/abc-123";
    let fragment = http::resource_uri(uri);
    let path = format!("{}/resources/{}", openapi::API_PREFIX, fragment);
    match http::resolve_route(&path) {
        Some(http::Route::ReadResource(rest)) => {
            assert!(rest.contains("abc-123"), "id lost in {rest:?}");
        }
        other => panic!("{path} resolved to {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 4. Prompts
// ---------------------------------------------------------------------------

#[test]
fn every_prompt_is_listed_by_both_transports() {
    let list = prompts::prompts_list();
    let rows = list.as_array().expect("prompts_list must return an array");
    assert!(!rows.is_empty());

    let mcp_list = mcp::prompts_list();
    for row in rows {
        let name = row["name"].as_str().unwrap();
        assert!(
            prompts::find_prompt(name).is_some(),
            "listed prompt {name} is not findable"
        );
        let in_mcp = mcp_list["prompts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"].as_str() == Some(name));
        assert!(in_mcp, "prompt {name} missing from MCP prompts/list");
    }
}

#[test]
fn prompts_reference_only_tools_that_exist() {
    let known = tool_names();
    for row in prompts::prompts_list().as_array().unwrap() {
        let name = row["name"].as_str().unwrap();
        let spec = prompts::find_prompt(name).expect("listed prompt must resolve");
        let args = std::collections::HashMap::new();
        let text = (spec.render)(&args);
        // Every snake_case identifier that looks like a tool name must be real.
        for candidate in [
            "list_services", "get_service", "get_settings", "get_state",
            "list_recipes", "get_service_icon", "list_sessions",
            "add_service", "update_service", "patch_settings", "reorder_services",
            "set_active_service", "reload_service", "navigate", "navigate_url",
            "hibernate_service", "wake_service", "reset_service_icon",
            "remove_service", "export_config", "import_config",
        ] {
            if text.contains(candidate) {
                assert!(
                    known.contains(&candidate),
                    "prompt {name} references {candidate}, which is not a registered tool"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 5. The privilege ceiling
// ---------------------------------------------------------------------------

#[test]
fn destructive_tools_are_admin_only() {
    for name in ["remove_service", "import_config"] {
        let tool = tools::find_tool(name).unwrap_or_else(|| panic!("{name} must exist"));
        assert_eq!(
            tool.scope.as_str(),
            "admin",
            "{name} is irreversible and must require an admin token"
        );
    }
}

#[test]
fn no_read_tool_is_marked_mutating() {
    for tool in tools::all_tools() {
        if tool.scope.as_str() == "read" {
            assert!(!tool.mutating, "{} is read-scoped but mutating", tool.name);
        }
    }
}

#[test]
fn ui_only_commands_are_absent_from_every_surface() {
    let forbidden = [
        "set_overlay_mode",
        "open_devtools",
        "open_service_external",
        "report_title",
        "pick_service_icon",
    ];
    let doc = openapi::document();
    let mcp_text = mcp::tools_list().to_string();
    for name in forbidden {
        assert!(
            tools::find_tool(name).is_none(),
            "{name} is exposed as a tool"
        );
        assert!(
            !doc["paths"].as_object().unwrap().contains_key(&format!("/api/v1/agent/tools/{name}")),
            "{name} appears in the OpenAPI document"
        );
        assert!(
            !mcp_text.contains(&format!("\"{name}\"")),
            "{name} appears in the MCP tool list"
        );
    }
}

// ---------------------------------------------------------------------------
// 6. Schema quality: an LLM must be able to fill every argument unaided
// ---------------------------------------------------------------------------

#[test]
fn every_tool_schema_describes_every_property() {
    for tool in tools::all_tools() {
        let schema = (tool.input_schema)();
        let props = schema["properties"].as_object();
        let Some(props) = props else { continue };
        for (prop, spec) in props {
            let described = spec
                .get("description")
                .and_then(Value::as_str)
                .map(|d| !d.trim().is_empty())
                .unwrap_or(false);
            assert!(
                described,
                "tool {} property {prop} has no description; an LLM cannot fill it reliably",
                tool.name
            );
        }
    }
}

#[test]
fn required_arguments_exist_in_the_schema() {
    for tool in tools::all_tools() {
        let schema = (tool.input_schema)();
        if let Some(required) = schema["required"].as_array() {
            for r in required {
                let name = r.as_str().unwrap();
                assert!(
                    schema["properties"].get(name).is_some(),
                    "tool {} requires {name} but does not declare it",
                    tool.name
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 7. Protocol basics
// ---------------------------------------------------------------------------

#[test]
fn mcp_negotiates_a_version_it_advertises() {
    for requested in mcp::SUPPORTED_PROTOCOL_VERSIONS {
        let got = mcp::negotiate_version(Some(requested));
        assert_eq!(got, requested, "a client's supported version must be honoured");
        assert!(mcp::SUPPORTED_PROTOCOL_VERSIONS.contains(&got));
    }
    // An unknown or absent request must still land on a supported version.
    let fallback = mcp::negotiate_version(Some("1999-01-01"));
    assert!(mcp::SUPPORTED_PROTOCOL_VERSIONS.contains(&fallback));
    let absent = mcp::negotiate_version(None);
    assert!(mcp::SUPPORTED_PROTOCOL_VERSIONS.contains(&absent));
}

#[test]
fn openapi_document_is_a_valid_3_1_root() {
    let doc = openapi::document();
    assert_eq!(doc["openapi"], "3.1.0");
    assert!(doc["info"]["title"].is_string());
    assert!(doc["info"]["version"].is_string());
    assert!(doc["paths"].is_object());
    assert!(
        doc["components"]["schemas"].is_object(),
        "3.1 documents should publish reusable schemas"
    );
    // 3.1 identifies itself via jsonSchemaDialect rather than requiring it,
    // but any $ref must point somewhere we actually define.
    let text = doc.to_string();
    assert!(!text.contains("#/components/schemas/Undefined"));
}

#[test]
fn error_envelope_shape_is_identical_on_both_transports() {
    use bzdium_lib::agent::error::{AgentError, ErrorCode};
    let err = AgentError::new(ErrorCode::UnknownTool, "no such tool", "call tools/list");

    // REST: an HTTP error body.
    let resp = http::error_response(&err);
    assert_eq!(resp.status().as_u16(), err.status());
    assert!(resp.headers().get("www-authenticate").is_none());

    // MCP: an in-band error result for a tool failure.
    let mcp_err = mcp::tool_error_result(&err);
    assert_eq!(mcp_err["isError"], true);
    let text = mcp_err["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).expect("MCP tool error must embed JSON");
    assert_eq!(parsed["code"], "UNKNOWN_TOOL");
    assert!(parsed["suggestion"].is_string());
}

#[test]
fn a_401_always_carries_www_authenticate_but_a_403_never_does() {
    use bzdium_lib::agent::error::{AgentError, ErrorCode};
    let unauth = AgentError::new(ErrorCode::Unauthorized, "no token", "send one");
    let resp = http::error_response(&unauth);
    assert_eq!(resp.status().as_u16(), 401);
    assert!(
        resp.headers().get("www-authenticate").is_some(),
        "a 401 must tell a well-behaved client how to authenticate"
    );

    let forbidden = AgentError::new(ErrorCode::ForbiddenScope, "hostile origin", "use a native client");
    let resp = http::error_response(&forbidden);
    assert_eq!(resp.status().as_u16(), 403);
    assert!(
        resp.headers().get("www-authenticate").is_none(),
        "a 403 must not invite the caller to retry with credentials"
    );
}

#[test]
fn no_response_ever_emits_cors_headers() {
    use bzdium_lib::agent::error::{AgentError, ErrorCode};
    for err in [
        AgentError::new(ErrorCode::Unauthorized, "x", "y"),
        AgentError::new(ErrorCode::ForbiddenScope, "x", "y"),
        AgentError::new(ErrorCode::Internal, "x", "y"),
    ] {
        let resp = http::error_response(&err);
        for (name, _) in resp.headers() {
            let n = name.as_str().to_ascii_lowercase();
            assert!(
                !n.starts_with("access-control-"),
                "response leaked CORS header {n}; the API must never be reachable from a browser"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 8. The host allow-list, which is what defeats DNS rebinding
// ---------------------------------------------------------------------------

#[test]
fn host_allow_list_accepts_loopback_and_refuses_everything_else() {
    for good in ["127.0.0.1", "127.0.0.1:8765", "localhost", "localhost:8765"] {
        assert!(http::host_is_allowed(good), "{good:?} should be allowed");
    }
    for bad in [
        "",
        "evil.com",
        "evil.com:8765",
        "127.0.0.1.evil.com",
        "0.0.0.0:8765",
        "192.168.1.5:8765",
        "localhost.evil.com",
    ] {
        assert!(!http::host_is_allowed(bad), "{bad:?} must be refused");
    }
}

#[test]
fn bearer_parsing_is_scheme_case_insensitive_but_value_exact() {
    assert_eq!(http::bearer_token("Bearer bzd_agt_x"), Some("bzd_agt_x"));
    assert_eq!(http::bearer_token("bearer bzd_agt_x"), Some("bzd_agt_x"));
    assert_eq!(http::bearer_token("BEARER bzd_agt_x"), Some("bzd_agt_x"));
    assert_eq!(http::bearer_token("Basic abc"), None);
    assert_eq!(http::bearer_token("bzd_agt_x"), None);
    assert_eq!(http::bearer_token("Bearer "), None);
}

#[test]
fn session_resource_declares_only_names_and_sizes() {
    // The sessions resource DESCRIPTION legitimately names the things it
    // refuses to return (it promises they are never exposed), so a substring
    // scan over the whole row is the wrong assertion. What matters is the
    // declared payload shape: the manifest may only promise id + size.
    let rows = resources::resources_list().as_array().unwrap().clone();
    let sessions = rows
        .iter()
        .find(|r| r["uri"].as_str() == Some("bzdium://sessions"))
        .expect("bzdium://sessions must be registered");

    let desc = sessions["description"].as_str().unwrap().to_ascii_lowercase();
    assert!(
        desc.contains("bytes") || desc.contains("size"),
        "the sessions resource must declare that it returns sizes"
    );
    // It must be explicit that it is names-and-sizes ONLY.
    assert!(
        desc.contains("only") || desc.contains("never"),
        "the sessions description must state the contents are excluded"
    );

    // No other resource may claim to expose profile material.
    for row in rows {
        if row["uri"].as_str() == Some("bzdium://sessions") {
            continue;
        }
        let d = row["description"].as_str().unwrap().to_ascii_lowercase();
        for banned in ["cookie", "localstorage", "credential"] {
            assert!(
                !d.contains(banned),
                "resource {} mentions {banned}",
                row["uri"]
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 8. The IPC surface the UI needs is actually permitted
// ---------------------------------------------------------------------------

/// Tauri v2 denies every command that is not both declared to `tauri-build`
/// (which generates the `allow-<command>` permission) and granted in a
/// capability. Declaring a command in one place and forgetting the other
/// compiles cleanly, links cleanly, and then fails at runtime the first time
/// the human clicks the control -- which is exactly how the agent panel's
/// bootstrap shipped broken once. Pin both halves against the same list.
#[test]
fn every_agent_command_is_declared_and_permitted() {
    let wanted = [
        "get_agent_status",
        "set_agent_admin_scope",
        "mint_agent_token",
        "revoke_agent_tokens",
    ];

    let build_rs = std::fs::read_to_string("build.rs").expect("build.rs must be readable");
    let capability = std::fs::read_to_string("capabilities/default.json")
        .expect("capabilities/default.json must be readable");
    let capability: Value =
        serde_json::from_str(&capability).expect("the capability must be valid JSON");
    let granted: Vec<&str> = capability["permissions"]
        .as_array()
        .expect("the capability must list permissions")
        .iter()
        .filter_map(|p| p.as_str())
        .collect();

    for command in wanted {
        assert!(
            build_rs.contains(&format!("\"{command}\"")),
            "build.rs must declare {command} so its allow-permission exists"
        );
        // Tauri maps command `foo_bar` to the permission `allow-foo-bar`.
        let permission = format!("allow-{}", command.replace('_', "-"));
        assert!(
            granted.contains(&permission.as_str()),
            "capabilities/default.json must grant {permission}, or the UI call is denied"
        );
    }
}

/// Every command the UI can reach must be listed, not just the agent ones: a
/// command present in build.rs but absent from the capability is dead code
/// that fails only at runtime.
#[test]
fn no_declared_command_is_missing_from_the_capability() {
    let build_rs = std::fs::read_to_string("build.rs").expect("build.rs must be readable");

    // A command may legitimately be granted by any capability, not just the
    // local one: report_title is granted only to the remote service webviews,
    // because that is the only origin that needs it. So collect permissions
    // across every capability file and check the union.
    let mut granted: Vec<String> = Vec::new();
    let mut files = 0usize;
    for entry in std::fs::read_dir("capabilities").expect("capabilities/ must be readable") {
        let path = entry.expect("capability dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("capability file must be readable");
        let parsed: Value = serde_json::from_str(&text).expect("capability must be valid JSON");
        if let Some(perms) = parsed["permissions"].as_array() {
            granted.extend(perms.iter().filter_map(|p| p.as_str()).map(str::to_string));
        }
        files += 1;
    }
    assert!(files >= 2, "expected at least the local and remote capabilities");

    // Pull the command names out of the `.commands(&[...])` block.
    let start = build_rs.find(".commands(&[").expect("build.rs must declare commands");
    let block = &build_rs[start..];
    let end = block.find("])").expect("the command list must be closed");

    let mut checked = 0usize;
    for token in block[..end].split('"') {
        let name = token.trim();
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            let permission = format!("allow-{}", name.replace('_', "-"));
            assert!(
                granted.contains(&permission),
                "{name} is declared in build.rs but {permission} is not granted, so the UI cannot call it"
            );
            checked += 1;
        }
    }
    assert!(checked >= 20, "expected to check the full command list, saw {checked}");
}

/// The frontend and the backend have to agree in both directions. This pins
/// the direction that fails silently: a command the UI calls but no capability
/// grants compiles, links, and then throws 'not allowed' the moment a human
/// clicks the control. Reading the real app.js means the test cannot drift out
/// of date the way a hand-maintained list would.
#[test]
fn every_command_the_ui_calls_is_granted() {
    let ui = std::fs::read_to_string("../dist/app.js").expect("dist/app.js must be readable");

    // Collect cmd("name") call sites.
    let mut called: Vec<String> = Vec::new();
    let needle = "cmd(\"";
    let mut rest = ui.as_str();
    while let Some(i) = rest.find(needle) {
        let after = &rest[i + needle.len()..];
        if let Some(end) = after.find('"') {
            let name = &after[..end];
            if !name.is_empty() {
                called.push(name.to_string());
            }
            rest = &after[end..];
        } else {
            break;
        }
    }
    called.sort();
    called.dedup();
    assert!(
        called.len() >= 20,
        "expected to find the UI's command calls, only saw {}: {:?}",
        called.len(),
        called
    );

    let mut granted: Vec<String> = Vec::new();
    for entry in std::fs::read_dir("capabilities").expect("capabilities/ must be readable") {
        let path = entry.expect("capability dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("capability must be readable");
        let parsed: Value = serde_json::from_str(&text).expect("capability must be valid JSON");
        if let Some(perms) = parsed["permissions"].as_array() {
            granted.extend(perms.iter().filter_map(|p| p.as_str()).map(str::to_string));
        }
    }

    let mut missing: Vec<String> = Vec::new();
    for command in &called {
        let permission = format!("allow-{}", command.replace('_', "-"));
        if !granted.contains(&permission) {
            missing.push(format!("{command} (needs {permission})"));
        }
    }
    assert!(
        missing.is_empty(),
        "the UI calls commands that no capability grants, so they fail at runtime: {missing:?}"
    );
}

